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
  type AutomaticMeeting,
} from "./automatic-meeting";
import {
  replayNativeRecordingExclusions,
  stopMeetingRecordingForExcludedOccurrence,
} from "./meeting-recording-control";
import {
  bindRecordingSession,
  isRecordingExcluded,
  resolveRecordingOccurrence,
} from "./recording-exclusions";

import { recordRecordingDiagnostic } from "~/application-health/recording-diagnostics";
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
      missingSince: number | null;
      missingObservation: "inactive" | "unknown" | null;
      lastAudibleAt: number;
      networkHoldUntil: number | null;
    } | null = null;
    const suppressed = new Map<string, AutomaticMeeting>();
    let retryAfter = 0;
    let probeStartedAt = 0;
    let phase: "probe" | "sharing" | "recording" = "probe";
    let probeEvidence: NonNullable<
      Parameters<typeof recordRecordingDiagnostic>[3]
    > = {};
    const diagnose = (
      kind: string,
      reason: string,
      evidence: Parameters<typeof recordRecordingDiagnostic>[3] = {},
      sessionId = capture?.sessionId ?? store.getState().live.sessionId,
    ) => {
      recordRecordingDiagnostic(sessionId, kind, reason, {
        platform: capture?.meeting.platform ?? "unknown",
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
        diagnose("policy_decision", "observed", { owned: false });
        suppressed.set(capture.meeting.key, capture.meeting);
        capture = null;
        ownedSession.current = null;
      }
    });

    async function reconcile() {
      if (cancelled || checking) return;
      if (!identityReady.current) {
        diagnose("policy_decision", "identity_unavailable");
        return;
      }
      if (!policy.current.enabled) {
        diagnose("policy_decision", "auto_start_disabled");
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
      diagnose("detector_observation", "probe_started", probeEvidence);
      try {
        const [inspection, microphones] = await Promise.all([
          detectCommands.inspectMeetingAccessibility().then(
            (result) => {
              currentProbe.inspectionStatus = result.status;
              return result;
            },
            (error) => {
              currentProbe.inspectionStatus = "error";
              throw error;
            },
          ),
          detectCommands.listMicUsingApplications().then(
            (result) => {
              currentProbe.micStatus = result.status;
              return result;
            },
            (error) => {
              currentProbe.micStatus = "error";
              throw error;
            },
          ),
        ]);
        if (cancelled) return;
        if (!policy.current.enabled) {
          diagnose("policy_decision", "auto_start_disabled", probeEvidence);
          return;
        }
        if (inspection.status !== "ok") {
          diagnose("detector_observation", "ax_error", probeEvidence);
          return;
        }
        const selected = selectAutomaticMeeting(
          inspection.data.filter(
            (entry) => !policy.current.ignored?.includes(entry.app.id),
          ),
          {
            googleMeet: policy.current.enabled,
            slack: policy.current.enabled,
          },
        );
        if (!capture)
          diagnose("detector_observation", "observed", {
            ...probeEvidence,
            observation: selected ? "active" : "unknown",
            platform: selected?.platform ?? "unknown",
            probeElapsedMs: Date.now() - probeStartedAt,
          });
        const now = Date.now();
        for (const [key, meeting] of suppressed) {
          if (
            automaticMeetingObservation(meeting, inspection.data) ===
              "inactive" ||
            (platform() === "macos" &&
              !inspection.data.some(
                (entry) =>
                  entry.app.id === meeting.app.id && entry.pid === meeting.pid,
              ))
          )
            suppressed.delete(key);
        }

        const owner = account.current;
        phase = "sharing";
        if (owner) await replayNativeRecordingExclusions(owner);
        if (cancelled) return;
        if (!identityReady.current || account.current !== owner) {
          diagnose("policy_decision", "account_changed", probeEvidence);
          return;
        }
        const occurrence =
          owner && selected
            ? await resolveRecordingOccurrence(owner, { meeting: selected })
            : null;
        if (cancelled) return;
        if (!identityReady.current || account.current !== owner) {
          diagnose("policy_decision", "account_changed", probeEvidence);
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
            diagnose("stop_requested", "recording_excluded", probeEvidence);
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
              inspection.data,
            );
            const macMeet =
              capture.meeting.platform === "googleMeet" &&
              platform() === "macos";
            if (
              observation === "unknown" &&
              macMeet &&
              !inspection.data.some(
                (entry) =>
                  entry.app.id === capture!.meeting.app.id &&
                  entry.pid === capture!.meeting.pid,
              )
            ) {
              // macOS returns a row for every recognized running process,
              // including unreadable ones. Missing that process is a departure.
              observation = "inactive";
            }
            const micActive =
              microphones.status !== "ok" ||
              microphones.data.some(
                (app) => app.id === capture!.meeting.app.id,
              );
            const recordingEvidence = inspection.data.find(
              (row) =>
                row.app.id === capture!.meeting.app.id &&
                row.pid === capture!.meeting.pid &&
                row.platform === capture!.meeting.platform,
            )?.recordingEvidence;
            const evidence = {
              ...probeEvidence,
              ...(recordingEvidence && {
                leaveControlPresent: recordingEvidence.leaveControlPresent,
                leaveControlVisible: recordingEvidence.leaveControlVisible,
                matchedBy: recordingEvidence.matchedBy,
                ...(typeof recordingEvidence.leaveControlEnabled ===
                  "boolean" && {
                  leaveControlEnabled: recordingEvidence.leaveControlEnabled,
                }),
              }),
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
            diagnose("detector_observation", "observed", evidence);
            if (observation === "unknown" && !navigator.onLine)
              capture.networkHoldUntil ??= now + AUTO_STOP_NETWORK_HOLD_MS;
            if (
              observation === "active" ||
              (observation === "unknown" && (macMeet || micActive))
            ) {
              // A hidden browser tab can lose AX evidence and microphone
              // ownership while its muted call continues. Only confirmed
              // departure can end an established Meet capture.
              // An unreadable Meet poll cannot stop capture. Preserve a prior
              // confirmed departure so another fresh inactive poll can finish
              // its grace; positive attendance resets that evidence.
              if (observation === "active" || !macMeet) {
                capture.missingSince = null;
                capture.missingObservation = null;
              }
              if (observation === "active") capture.networkHoldUntil = null;
              diagnose(
                "policy_decision",
                policy.current.autoStop === false
                  ? "auto_stop_disabled"
                  : observation === "active"
                    ? "meeting_active"
                    : macMeet
                      ? "ax_unknown"
                      : "mic_still_active",
                evidence,
              );
              return;
            }
            if (capture.missingObservation !== observation) {
              capture.missingSince = now;
              capture.missingObservation = observation;
            }
            capture.missingSince ??= now;
            if (observation === "unknown") {
              if (
                capture.networkHoldUntil !== null &&
                now < capture.networkHoldUntil
              ) {
                diagnose("policy_decision", "network_recovery_hold", {
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
              diagnose("policy_decision", "ax_unknown", evidence);
              return;
            }
            if (
              policy.current.autoStop !== false &&
              now - capture.missingSince >=
                (observation === "unknown"
                  ? AUTOMATIC_MEETING_UNKNOWN_GRACE_MS
                  : AUTOMATIC_MEETING_STOP_GRACE_MS)
            ) {
              diagnose("stop_requested", "grace_elapsed", {
                ...evidence,
                missingForMs: now - capture.missingSince,
              });
              capture = null;
              ownedSession.current = null;
              store.getState().stop();
            } else {
              diagnose(
                "policy_decision",
                policy.current.autoStop === false
                  ? "auto_stop_disabled"
                  : "grace_pending",
                { ...evidence, missingForMs: now - capture.missingSince },
              );
            }
            return;
          }
          if (capture.seenActive) {
            diagnose("policy_decision", "owner_mismatch", probeEvidence);
            return;
          }
          if (now - capture.startedAt < 35_000) {
            diagnose("policy_decision", "grace_pending", probeEvidence);
            return;
          }
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
              diagnose("stop_requested", "probe_timeout", probeEvidence);
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
          diagnose("policy_decision", "owner_mismatch", {
            ...probeEvidence,
            owned: false,
          });
        if (
          !selected ||
          [...suppressed.values()].some((meeting) =>
            sameAutomaticMeeting(meeting, selected),
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
              missingSince: null,
              missingObservation: null,
              lastAudibleAt: now,
              networkHoldUntil: null,
            };
            ownedSession.current = live.sessionId;
            diagnose("policy_decision", "observed", {
              ...probeEvidence,
              owned: true,
            });
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
            missingSince: null,
            missingObservation: null,
            lastAudibleAt: Date.now(),
            networkHoldUntil: null,
          };
          ownedSession.current = sessionId;
          diagnose("policy_decision", "observed", {
            ...probeEvidence,
            owned: true,
          });
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
        diagnose(
          "detector_observation",
          phase === "sharing" ? "sharing_lookup_failed" : "probe_error",
          { ...probeEvidence, probeElapsedMs: Date.now() - probeStartedAt },
        );
        retryAfter = Date.now() + 5_000;
        console.warn(
          "[listener] automatic meeting detection unavailable",
          error,
        );
      } finally {
        checking = false;
      }
    }

    const watchdog = setInterval(() => {
      if (!checking || cancelled) return;
      const elapsed = Date.now() - probeStartedAt;
      if (elapsed < 10_000) return;
      diagnose(
        "detector_observation",
        phase === "sharing"
          ? "sharing_lookup_pending"
          : probeEvidence.inspectionStatus === "pending"
            ? "ax_probe_timeout"
            : "probe_timeout",
        { ...probeEvidence, probeElapsedMs: elapsed },
      );
    }, 10_000);
    void reconcile();
    const interval = setInterval(
      () => void reconcile(),
      AUTOMATIC_MEETING_POLL_MS,
    );
    return () => {
      cancelled = true;
      clearInterval(interval);
      clearInterval(watchdog);
      unsubscribe();
    };
  });

  return { policy, ownedSession, reset };
}
