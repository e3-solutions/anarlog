import { platform } from "@tauri-apps/plugin-os";
import { useRef } from "react";

import { commands as detectCommands } from "@anlg/plugin-detect";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import {
  AUTOMATIC_MEETING_POLL_MS,
  AUTOMATIC_MEETING_STOP_GRACE_MS,
  automaticMeetingObservation,
  selectAutomaticMeeting,
  sameAutomaticMeeting,
  sameSuppressedAutomaticMeeting,
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
      inactiveSince: number | null;
    } | null = null;
    const suppressed = new Map<
      string,
      { meeting: AutomaticMeeting; untilAudioReturns: boolean }
    >();
    let pendingInspection: ReturnType<
      typeof detectCommands.inspectMeetingAccessibility
    > | null = null;
    let completedInspection: {
      result: Awaited<
        ReturnType<typeof detectCommands.inspectMeetingAccessibility>
      >;
      startedAt: number;
    } | null = null;
    let inspectionStartedAt = 0;
    const requestInspection = () => {
      if (!pendingInspection) {
        const startedAt = Date.now();
        inspectionStartedAt = startedAt;
        pendingInspection = detectCommands
          .inspectMeetingAccessibility()
          .catch((error) => ({
            status: "error" as const,
            error: String(error),
          }))
          .then((result) => {
            if (!cancelled) completedInspection = { result, startedAt };
            pendingInspection = null;
            return result;
          });
      }
      return pendingInspection;
    };
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

    let exclusionCheck: {
      owner: string | undefined;
      sessionId: string;
    } | null = null;
    async function checkAdmission(
      meeting: AutomaticMeeting | null,
      sessionId: string | null,
    ) {
      const owner = account.current;
      const current = () =>
        !cancelled &&
        identityReady.current &&
        account.current === owner &&
        (!sessionId || capture?.sessionId === sessionId);
      if (!current()) return null;
      if (owner) await replayNativeRecordingExclusions(owner);
      if (!current()) return null;
      const occurrence =
        owner && meeting
          ? await resolveRecordingOccurrence(owner, { meeting })
          : null;
      if (!current()) return null;
      if (owner && occurrence && isRecordingExcluded(owner, occurrence)) {
        if (sessionId && store.getState().live.sessionId === sessionId) {
          report("recording_excluded", {}, "stop_requested");
          capture = null;
          ownedSession.current = null;
          try {
            await stopMeetingRecordingForExcludedOccurrence(
              owner,
              sessionId,
              occurrence,
            );
          } finally {
            if (
              account.current === owner &&
              store.getState().live.sessionId === sessionId
            )
              store.getState().stop();
          }
        }
        return null;
      }
      return { owner, occurrence };
    }

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
      const macOS = platform() === "macos";
      const currentProbe: typeof probeEvidence = {
        inspectionStatus: "pending",
        micStatus: macOS ? "pending" : "unavailable",
      };
      probeEvidence = currentProbe;
      try {
        const inspecting = requestInspection();
        const [, audioProcesses] = await Promise.all([
          capture ? Promise.resolve() : inspecting,
          macOS
            ? detectCommands.listMeetingAudioProcesses().catch((error) => ({
                status: "error" as const,
                error: String(error),
              }))
            : Promise.resolve({
                status: "error" as const,
                error: "unsupported",
              }),
        ]);
        if (macOS) currentProbe.micStatus = audioProcesses.status;
        const completed = completedInspection;
        completedInspection = null;
        const freshInspection =
          completed && Date.now() - completed.startedAt < 10_000;
        const inspection = freshInspection
          ? completed.result
          : {
              status: "error" as const,
              error: "AX inspection pending or expired",
            };
        currentProbe.inspectionStatus = freshInspection
          ? inspection.status
          : Date.now() - inspectionStartedAt >= 10_000
            ? "timeout"
            : "pending";
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

        const live = store.getState().live;
        const reconciledCapture = Boolean(
          capture &&
          live.status === "active" &&
          live.sessionId === capture.sessionId,
        );
        if (capture && reconciledCapture) {
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
          if (
            observation === "unknown" &&
            macOS &&
            inspection.status === "ok" &&
            !inspections.some(
              (entry) =>
                entry.app.id === capture!.meeting.app.id &&
                entry.pid === capture!.meeting.pid,
            )
          )
            observation = "inactive";
          const inputActive =
            audioProcesses.status === "ok"
              ? audioProcesses.data.some(
                  (process) =>
                    process.app.id === capture!.meeting.app.id &&
                    process.inputActive,
                )
              : null;
          if (inputActive === true) {
            capture.seenInputActive = true;
            capture.inputReleasedSince = null;
          } else if (inputActive === false && capture.seenInputActive)
            capture.inputReleasedSince ??= now;
          if (observation === "active") capture.inactiveSince = null;
          else if (observation === "inactive") capture.inactiveSince ??= now;
          const uiEndedSince =
            observation === "inactive" ? capture.inactiveSince : null;
          const inputEndedSince =
            inputActive === false ? capture.inputReleasedSince : null;
          const endedSince =
            uiEndedSince === null
              ? inputEndedSince
              : inputEndedSince === null
                ? uiEndedSince
                : Math.min(uiEndedSince, inputEndedSince);
          const evidence = {
            observation,
            ...(inputActive === null ? {} : { micActive: inputActive }),
            missingForMs: endedSince === null ? 0 : now - endedSince,
          };
          if (
            policy.current.autoStop !== false &&
            endedSince !== null &&
            now - endedSince >= AUTOMATIC_MEETING_STOP_GRACE_MS
          ) {
            report("grace_elapsed", evidence, "stop_requested");
            if (
              inputEndedSince !== null &&
              (uiEndedSince === null || inputEndedSince <= uiEndedSince)
            )
              suppressed.set(capture.meeting.key, {
                meeting: capture.meeting,
                untilAudioReturns: true,
              });
            capture = null;
            ownedSession.current = null;
            store.getState().stop();
            return;
          }
          report(
            policy.current.autoStop === false
              ? "auto_stop_disabled"
              : endedSince !== null
                ? "grace_pending"
                : observation === "active"
                  ? "meeting_active"
                  : inputActive === true
                    ? "mic_still_active"
                    : "ax_unknown",
            evidence,
          );
        }

        if (capture) {
          if (
            exclusionCheck?.owner !== account.current ||
            exclusionCheck?.sessionId !== capture.sessionId
          ) {
            const check = {
              owner: account.current,
              sessionId: capture.sessionId,
            };
            exclusionCheck = check;
            const meetings = [capture.meeting];
            if (selected && !sameAutomaticMeeting(capture.meeting, selected))
              meetings.push(selected);
            void (async () => {
              for (const meeting of meetings) {
                if (account.current !== check.owner) return;
                await checkAdmission(meeting, check.sessionId);
              }
            })()
              .catch((error) => {
                report("sharing_lookup_failed");
                console.warn(
                  "[listener] recording exclusions unavailable",
                  error,
                );
              })
              .finally(() => {
                if (exclusionCheck === check) exclusionCheck = null;
              });
          }
          if (reconciledCapture) return;
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
        phase = "sharing";
        const admission = await checkAdmission(selected, null);
        if (!admission) return;
        const { owner, occurrence } = admission;
        phase = "recording";
        const available = store.getState().live;
        if (available.status === "active") {
          if (
            available.sessionId &&
            available.triggerAppIds?.includes(selected.app.id)
          ) {
            capture = {
              meeting: selected,
              sessionId: available.sessionId,
              startedAt: now,
              seenActive: true,
              seenInputActive:
                audioProcesses.status === "ok" &&
                audioProcesses.data.some(
                  (process) =>
                    process.app.id === selected.app.id && process.inputActive,
                ),
              inputReleasedSince: null,
              inactiveSince: null,
            };
            ownedSession.current = available.sessionId;
          }
          return;
        }
        if (available.loading || now < retryAfter) return;

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
          completedInspection = null;
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
            inactiveSince: null,
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
