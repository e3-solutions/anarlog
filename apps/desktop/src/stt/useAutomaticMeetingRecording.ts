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
  type AutomaticMeeting,
} from "./automatic-meeting";

import { createSession, softDeleteSession } from "~/session/queries";
import { useConfigValue } from "~/shared/config";
import type { ListenerStore } from "~/store/zustand/listener";
import { useTabs } from "~/store/zustand/tabs";

export function useAutomaticMeetingRecording(store: ListenerStore) {
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
        suppressed.set(capture.meeting.key, capture.meeting);
        capture = null;
        ownedSession.current = null;
      }
    });

    async function reconcile() {
      if (cancelled || checking) return;
      if (!policy.current.enabled) {
        reset.current();
        return;
      }
      checking = true;
      try {
        const [inspection, microphones] = await Promise.all([
          detectCommands.inspectMeetingAccessibility(),
          detectCommands.listMicUsingApplications(),
        ]);
        if (cancelled || !policy.current.enabled || inspection.status !== "ok")
          return;
        const selected = selectAutomaticMeeting(
          inspection.data.filter(
            (entry) => !policy.current.ignored?.includes(entry.app.id),
          ),
          {
            googleMeet: policy.current.enabled,
            slack: policy.current.enabled,
          },
        );
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

        const live = store.getState().live;
        if (capture) {
          if (
            live.status === "active" &&
            live.sessionId === capture.sessionId
          ) {
            capture.seenActive = true;
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
            if (observation === "unknown" && !navigator.onLine)
              capture.networkHoldUntil ??= now + AUTO_STOP_NETWORK_HOLD_MS;
            if (
              observation === "active" ||
              (observation === "unknown" && (macMeet || micActive))
            ) {
              // A hidden browser tab can lose AX evidence and microphone
              // ownership while its muted call continues. Only confirmed
              // departure can end an established Meet capture.
              capture.missingSince = null;
              capture.missingObservation = null;
              if (observation === "active") capture.networkHoldUntil = null;
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
              )
                return;
            }
            if (
              observation === "unknown" &&
              now - capture.lastAudibleAt < AUTOMATIC_MEETING_UNKNOWN_GRACE_MS
            )
              return;
            if (
              policy.current.autoStop !== false &&
              now - capture.missingSince >=
                (observation === "unknown"
                  ? AUTOMATIC_MEETING_UNKNOWN_GRACE_MS
                  : AUTOMATIC_MEETING_STOP_GRACE_MS)
            ) {
              capture = null;
              ownedSession.current = null;
              store.getState().stop();
            }
            return;
          }
          if (capture.seenActive) return;
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
            if (live.sessionId === unusedSessionId) store.getState().stop();
            retryAfter = now + 5_000;
            return;
          }
          capture = null;
          ownedSession.current = null;
          await softDeleteSession(unusedSessionId);
          retryAfter = now + 5_000;
          return;
        }
        if (!selected || suppressed.has(selected.key)) return;
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
          }
          return;
        }
        if (live.loading || now < retryAfter) return;

        const sessionId = await createSession(selected.title);
        let opened = false;
        try {
          const refreshed = await detectCommands.inspectMeetingAccessibility();
          if (
            cancelled ||
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
