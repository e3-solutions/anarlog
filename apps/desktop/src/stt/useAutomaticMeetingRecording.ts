import { platform } from "@tauri-apps/plugin-os";
import { useRef } from "react";

import { commands as detectCommands } from "@anlg/plugin-detect";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import { AUTO_STOP_NETWORK_HOLD_MS } from "./auto-stop";
import {
  AUTOMATIC_MEETING_POLL_MS,
  AUTOMATIC_MEETING_STOP_GRACE_MS,
  AUTOMATIC_MEETING_UNKNOWN_GRACE_MS,
  automaticMeetingObservation,
  selectAutomaticMeeting,
  sameAutomaticMeeting,
  sameSuppressedAutomaticMeeting,
  type AutomaticMeeting,
} from "./automatic-meeting";
import { BROWSER_AUTO_STOP_APP_IDS } from "./meeting-apps";
import {
  replayNativeRecordingExclusions,
  stopMeetingRecordingForExcludedOccurrence,
} from "./meeting-recording-control";
import {
  bindRecordingSession,
  isRecordingExcluded,
  resolveRecordingOccurrence,
} from "./recording-exclusions";

import {
  recordRecordingDiagnostic,
  type RecordingDiagnosticEvidence,
} from "~/application-health/recording-diagnostics";
import { useOptionalAuth } from "~/auth";
import { createSession, softDeleteSession } from "~/session/queries";
import { useConfigValue } from "~/shared/config";
import type { ListenerStore } from "~/store/zustand/listener";
import { useTabs } from "~/store/zustand/tabs";

export function useAutomaticMeetingRecording(store: ListenerStore) {
  const auth = useOptionalAuth();
  const userId = auth?.session?.user.id;
  const identityReady = useRef(!auth || Boolean(userId));
  identityReady.current = !auth || Boolean(userId);
  const account = useRef(userId);
  account.current = userId;
  const enabled = useConfigValue("auto_start_detected_meetings");
  const ignored = useConfigValue("ignored_platforms");
  const autoStop = useConfigValue("auto_stop_meetings");
  const policy = useRef({ enabled, autoStop, ignored });
  policy.current = { enabled, autoStop, ignored };
  const reset = useRef(() => {});
  const ownedSession = useRef<string | null>(null);

  useMountEffect(() => {
    let cancelled = false;
    let checking = false;
    let capture: {
      meeting: AutomaticMeeting;
      sessionId: string;
      startedAt: number;
      seenActive: boolean;
      seenInputActive: boolean;
      inputReleasedSince: number | null;
      missingSince: number | null;
      missingObservation: "inactive" | "unknown" | null;
      lastAudibleAt: number;
      networkHoldUntil: number | null;
    } | null = null;
    const suppressed = new Map<
      string,
      { meeting: AutomaticMeeting; untilAudioReturns: boolean }
    >();
    let retryAfter = 0;
    let probeStartedAt = 0;
    let phase: "probe" | "sharing" | "recording" = "probe";
    let probeEvidence: RecordingDiagnosticEvidence = {};
    const report = (
      reason: string,
      evidence: RecordingDiagnosticEvidence = {},
      kind = "policy_decision",
    ) => {
      const sessionId = capture?.sessionId ?? store.getState().live.sessionId;
      if (!sessionId || cancelled) return;
      recordRecordingDiagnostic(sessionId, kind, reason, {
        ...probeEvidence,
        autoStart: Boolean(policy.current.enabled),
        autoStop: policy.current.autoStop !== false,
        identityReady: identityReady.current,
        online: navigator.onLine,
        owned: Boolean(capture && capture.sessionId === sessionId),
        ...evidence,
      });
    };

    reset.current = () => {
      capture = null;
      suppressed.clear();
      ownedSession.current = null;
    };

    const unsubscribe = store.subscribe((state, previous) => {
      if (!capture) return;
      if (
        state.live.status === "active" &&
        state.live.sessionId === capture.sessionId
      ) {
        capture.seenActive = true;
        if (state.live.amplitude.speaker > 0.01)
          capture.lastAudibleAt = Date.now();
      } else if (
        capture.seenActive &&
        previous.live.status === "active" &&
        previous.live.sessionId === capture.sessionId
      ) {
        // A manual stop must stand down until this call has actually ended.
        suppressed.set(capture.meeting.key, {
          meeting: capture.meeting,
          untilAudioReturns: false,
        });
        capture = null;
        ownedSession.current = null;
      }
    });

    async function reconcile() {
      if (cancelled) return;
      if (checking) {
        const probeElapsedMs = Date.now() - probeStartedAt;
        if (probeElapsedMs >= 10_000)
          report(
            phase === "sharing" ? "sharing_lookup_pending" : "probe_timeout",
            { probeElapsedMs },
          );
        return;
      }
      if (!identityReady.current) {
        report("identity_unavailable");
        return;
      }
      if (!policy.current.enabled) {
        report("auto_start_disabled");
        reset.current();
        return;
      }
      checking = true;
      probeStartedAt = Date.now();
      phase = "probe";
      const currentProbe: typeof probeEvidence = {
        inspectionStatus: "pending",
        micStatus: "pending",
      };
      probeEvidence = currentProbe;
      const track = <T extends { status: "ok" | "error" }>(
        request: Promise<T>,
        field: "inspectionStatus" | "micStatus",
      ) =>
        request.then(
          (result) => {
            currentProbe[field] = result.status;
            return result;
          },
          (error) => {
            currentProbe[field] = "error";
            return { status: "error" as const, error: String(error) };
          },
        );
      try {
        const [inspection, microphones, audioProcesses] = await Promise.all([
          track(
            detectCommands.inspectMeetingAccessibility(),
            "inspectionStatus",
          ),
          track(detectCommands.listMicUsingApplications(), "micStatus"),
          platform() === "macos"
            ? detectCommands.listMeetingAudioProcesses().catch((error) => ({
                status: "error" as const,
                error: String(error),
              }))
            : Promise.resolve({
                status: "error" as const,
                error: "unsupported",
              }),
        ]);
        if (
          cancelled ||
          !policy.current.enabled ||
          (inspection.status !== "ok" && !capture)
        ) {
          report(policy.current.enabled ? "ax_error" : "auto_start_disabled");
          return;
        }
        const inspections = inspection.status === "ok" ? inspection.data : [];
        const selected = selectAutomaticMeeting(
          inspections.filter(
            (entry) => !policy.current.ignored?.includes(entry.app.id),
          ),
          {
            googleMeet: policy.current.enabled,
            slack: policy.current.enabled,
            teams: policy.current.enabled,
          },
        );
        const now = Date.now();
        for (const [key, { meeting, untilAudioReturns }] of suppressed) {
          if (
            (untilAudioReturns &&
              audioProcesses.status === "ok" &&
              audioProcesses.data.some(
                (process) =>
                  process.app.id === meeting.app.id && process.inputActive,
              )) ||
            (inspection.status === "ok" &&
              (automaticMeetingObservation(meeting, inspections) ===
                "inactive" ||
                (platform() === "macos" &&
                  !inspections.some(
                    (entry) =>
                      entry.app.id === meeting.app.id &&
                      entry.pid === meeting.pid,
                  ))))
          )
            suppressed.delete(key);
        }

        const owner = account.current;
        phase = "sharing";
        if (owner) await replayNativeRecordingExclusions(owner);
        if (cancelled || !identityReady.current || account.current !== owner) {
          report("account_changed");
          return;
        }
        const occurrence =
          owner && selected
            ? await resolveRecordingOccurrence(owner, { meeting: selected })
            : null;
        if (cancelled || !identityReady.current || account.current !== owner) {
          report("account_changed");
          return;
        }
        phase = "recording";
        if (owner && occurrence && isRecordingExcluded(owner, occurrence)) {
          const live = store.getState().live;
          const blockedSession =
            capture && live.sessionId === capture.sessionId
              ? capture.sessionId
              : null;
          if (blockedSession) {
            report("recording_excluded", {}, "stop_requested");
            capture = null;
            ownedSession.current = null;
            try {
              await stopMeetingRecordingForExcludedOccurrence(
                owner,
                blockedSession,
                occurrence,
              );
            } finally {
              if (
                account.current === owner &&
                store.getState().live.sessionId === blockedSession
              )
                store.getState().stop();
            }
          }
          return;
        }
        const live = store.getState().live;
        if (capture) {
          if (
            live.status === "active" &&
            live.sessionId === capture.sessionId
          ) {
            capture.seenActive = true;
            if (
              selected &&
              sameAutomaticMeeting(capture.meeting, selected) &&
              selected.callIdentity?.includes(":tab:")
            )
              capture.meeting = selected;
            let observation = automaticMeetingObservation(
              capture.meeting,
              inspections,
            );
            const macBrowser =
              platform() === "macos" &&
              BROWSER_AUTO_STOP_APP_IDS.has(capture.meeting.app.id);
            if (
              observation === "unknown" &&
              macBrowser &&
              inspection.status === "ok" &&
              !inspections.some(
                (entry) =>
                  entry.app.id === capture!.meeting.app.id &&
                  entry.pid === capture!.meeting.pid,
              )
            ) {
              // macOS returns a row for every recognized running process,
              // including unreadable ones. Missing that process is a departure.
              observation = "inactive";
            }
            const matchingAudioProcesses =
              audioProcesses.status === "ok"
                ? audioProcesses.data.filter(
                    (process) => process.app.id === capture!.meeting.app.id,
                  )
                : [];
            const ownedAudioActive = matchingAudioProcesses.some(
              (process) => process.inputActive || process.outputActive,
            );
            const audioUnknown =
              platform() === "macos" && audioProcesses.status !== "ok";
            const micActive =
              platform() === "macos" && audioProcesses.status === "ok"
                ? matchingAudioProcesses.some((process) => process.inputActive)
                : microphones.status !== "ok" ||
                  microphones.data.some(
                    (app) => app.id === capture!.meeting.app.id,
                  );
            if (macBrowser && audioProcesses.status === "ok") {
              if (
                matchingAudioProcesses.some((process) => process.inputActive)
              ) {
                capture.seenInputActive = true;
                capture.inputReleasedSince = null;
                capture.networkHoldUntil = null;
              } else if (capture.seenInputActive) {
                capture.inputReleasedSince ??= now;
              }
            }
            const evidence = {
              observation,
              micActive,
              speakerRecent:
                now - capture.lastAudibleAt <
                AUTOMATIC_MEETING_UNKNOWN_GRACE_MS,
              missingForMs:
                capture.missingSince === null ? 0 : now - capture.missingSince,
              networkHoldRemainingMs: Math.max(
                0,
                (capture.networkHoldUntil ?? now) - now,
              ),
            };
            if (
              macBrowser &&
              audioProcesses.status === "ok" &&
              capture.inputReleasedSince !== null &&
              (observation !== "inactive" ||
                now - capture.inputReleasedSince >=
                  AUTOMATIC_MEETING_STOP_GRACE_MS)
            ) {
              const releasedForMs = now - capture.inputReleasedSince;
              if (observation !== "inactive" && !navigator.onLine)
                capture.networkHoldUntil ??= now + AUTO_STOP_NETWORK_HOLD_MS;
              if (
                observation !== "inactive" &&
                capture.networkHoldUntil !== null &&
                now < capture.networkHoldUntil
              ) {
                report("network_recovery_hold", {
                  ...evidence,
                  networkHoldRemainingMs: capture.networkHoldUntil - now,
                });
                return;
              }
              if (
                policy.current.autoStop !== false &&
                releasedForMs >= AUTOMATIC_MEETING_STOP_GRACE_MS
              ) {
                report(
                  "grace_elapsed",
                  { ...evidence, missingForMs: releasedForMs },
                  "stop_requested",
                );
                suppressed.set(capture.meeting.key, {
                  meeting: capture.meeting,
                  untilAudioReturns: true,
                });
                capture = null;
                ownedSession.current = null;
                store.getState().stop();
              } else {
                report(
                  policy.current.autoStop === false
                    ? "auto_stop_disabled"
                    : "grace_pending",
                  { ...evidence, missingForMs: releasedForMs },
                );
              }
              return;
            }
            if (observation === "unknown" && !navigator.onLine)
              capture.networkHoldUntil ??= now + AUTO_STOP_NETWORK_HOLD_MS;
            if (
              observation === "active" ||
              (observation === "unknown" &&
                (macBrowser || micActive || ownedAudioActive || audioUnknown))
            ) {
              // A browser started while muted has no input-release edge yet.
              // Preserve it through missing AX until a confirmed departure.
              // Keep prior departure timing so a fresh inactive poll can finish
              // its grace; positive attendance resets that evidence.
              if (observation === "active" || !macBrowser) {
                capture.missingSince = null;
                capture.missingObservation = null;
              }
              if (observation === "active") capture.networkHoldUntil = null;
              report(
                policy.current.autoStop === false
                  ? "auto_stop_disabled"
                  : observation === "active"
                    ? "meeting_active"
                    : micActive
                      ? "mic_still_active"
                      : "ax_unknown",
                evidence,
              );
              return;
            }
            if (capture.missingObservation !== observation) {
              capture.missingSince = now;
              capture.missingObservation = observation;
            }
            capture.missingSince ??= now;
            evidence.missingForMs = now - capture.missingSince;
            if (observation === "unknown") {
              if (
                capture.networkHoldUntil !== null &&
                now < capture.networkHoldUntil
              ) {
                report("network_recovery_hold", {
                  ...evidence,
                  networkHoldRemainingMs: capture.networkHoldUntil - now,
                });
                return;
              }
            }
            if (
              observation === "unknown" &&
              now - capture.lastAudibleAt < AUTOMATIC_MEETING_UNKNOWN_GRACE_MS
            ) {
              report("ax_unknown", evidence);
              return;
            }
            if (
              policy.current.autoStop !== false &&
              now - capture.missingSince >=
                (observation === "unknown"
                  ? AUTOMATIC_MEETING_UNKNOWN_GRACE_MS
                  : AUTOMATIC_MEETING_STOP_GRACE_MS)
            ) {
              report("grace_elapsed", evidence, "stop_requested");
              capture = null;
              ownedSession.current = null;
              store.getState().stop();
            } else {
              report(
                policy.current.autoStop === false
                  ? "auto_stop_disabled"
                  : "grace_pending",
                evidence,
              );
            }
            return;
          }
          if (capture.seenActive) {
            report("owner_mismatch");
            return;
          }
          if (now - capture.startedAt < 35_000) return;
          const unusedSessionId = capture.sessionId;
          const tabsState = useTabs.getState();
          const pendingTab = tabsState.tabs?.find(
            (tab) => tab.type === "sessions" && tab.id === unusedSessionId,
          );
          if (pendingTab?.type === "sessions") {
            tabsState.updateSessionTabState(pendingTab, {
              ...pendingTab.state,
              autoStart: null,
              scheduledAutoStart: null,
            });
          }
          if (live.loading) {
            if (live.sessionId === unusedSessionId) {
              report("probe_timeout", {}, "stop_requested");
              store.getState().stop();
            }
            retryAfter = now + 5_000;
            return;
          }
          capture = null;
          ownedSession.current = null;
          await softDeleteSession(unusedSessionId);
          retryAfter = now + 5_000;
          return;
        }
        if (live.status === "active" && !capture)
          report("owner_mismatch", { owned: false });
        if (
          !selected ||
          [...suppressed.values()].some(({ meeting }) =>
            sameSuppressedAutomaticMeeting(meeting, selected),
          )
        )
          return;
        if (live.status === "active") {
          if (live.sessionId && live.triggerAppIds?.includes(selected.app.id)) {
            capture = {
              meeting: selected,
              sessionId: live.sessionId,
              startedAt: now,
              seenActive: true,
              seenInputActive:
                audioProcesses.status === "ok" &&
                audioProcesses.data.some(
                  (process) =>
                    process.app.id === selected.app.id && process.inputActive,
                ),
              inputReleasedSince: null,
              missingSince: null,
              missingObservation: null,
              lastAudibleAt: now,
              networkHoldUntil: null,
            };
            ownedSession.current = live.sessionId;
          }
          return;
        }
        if (live.loading || now < retryAfter) return;

        const sessionId = await createSession(selected.title);
        if (owner && occurrence)
          bindRecordingSession(owner, sessionId, occurrence);
        let opened = false;
        try {
          const refreshed = await detectCommands.inspectMeetingAccessibility();
          if (
            cancelled ||
            !identityReady.current ||
            account.current !== owner ||
            (owner && occurrence && isRecordingExcluded(owner, occurrence)) ||
            !policy.current.enabled ||
            refreshed.status !== "ok" ||
            selectAutomaticMeeting(
              refreshed.data.filter(
                (entry) => !policy.current.ignored?.includes(entry.app.id),
              ),
              {
                googleMeet: policy.current.enabled,
                slack: policy.current.enabled,
                teams: policy.current.enabled,
              },
            )?.key !== selected.key ||
            !store.getState().canStartLiveSession(sessionId)
          )
            return;
          capture = {
            meeting: selected,
            sessionId,
            startedAt: Date.now(),
            seenActive: false,
            seenInputActive:
              audioProcesses.status === "ok" &&
              audioProcesses.data.some(
                (process) =>
                  process.app.id === selected.app.id && process.inputActive,
              ),
            inputReleasedSince: null,
            missingSince: null,
            missingObservation: null,
            lastAudibleAt: Date.now(),
            networkHoldUntil: null,
          };
          ownedSession.current = sessionId;
          store.getState().setTriggerAppIds([selected.app.id]);
          useTabs.getState().openNew({
            type: "sessions",
            id: sessionId,
            state: { view: null, autoStart: true, scheduledAutoStart: null },
          });
          opened = true;
        } finally {
          if (!opened) {
            capture = null;
            ownedSession.current = null;
            await softDeleteSession(sessionId);
            retryAfter = Date.now() + 5_000;
          }
        }
      } catch (error) {
        report(phase === "sharing" ? "sharing_lookup_failed" : "probe_error", {
          ...probeEvidence,
          probeElapsedMs: Date.now() - probeStartedAt,
        });
        retryAfter = Date.now() + 5_000;
        console.warn(
          "[listener] automatic meeting detection unavailable",
          error,
        );
      } finally {
        checking = false;
      }
    }

    void reconcile();
    const interval = setInterval(
      () => void reconcile(),
      AUTOMATIC_MEETING_POLL_MS,
    );
    return () => {
      cancelled = true;
      clearInterval(interval);
      unsubscribe();
    };
  });

  return { policy, ownedSession, reset };
}
