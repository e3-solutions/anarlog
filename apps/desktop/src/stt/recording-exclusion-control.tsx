import { useMutation, useQuery } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";

import { commands as detectCommands } from "@anlg/plugin-detect";

import { selectAutomaticMeeting } from "./automatic-meeting";
import { RecordingExclusionView } from "./recording-exclusion-view";

import { useOptionalAuth } from "~/auth";
import { createSession } from "~/session/queries";
import { excludeSharedRecording } from "~/shared-recordings/native";
import { listenerStore } from "~/store/zustand/listener/instance";
import { useTabs } from "~/store/zustand/tabs";
import {
  excludeRecordingOccurrence,
  getRecordingExclusionsSnapshot,
  getRecordingOccurrenceSessionIds,
  isRecordingExcluded,
  isRecordingSessionExcluded,
  resolveRecordingOccurrence,
  resumeRecordingOccurrence,
  subscribeRecordingExclusions,
} from "~/stt/recording-exclusions";

export function RecordingExclusionControl({
  sessionId,
  event,
}: {
  sessionId?: string;
  event?: {
    id?: string;
    calendar_id?: string;
    tracking_id?: string;
    tracking_id_event?: string;
    started_at?: string;
    ended_at?: string;
    meeting_link?: string;
  };
}) {
  const userId = useOptionalAuth()?.session?.user.id;
  const revision = useSyncExternalStore(
    subscribeRecordingExclusions,
    getRecordingExclusionsSnapshot,
  );
  const { data: resolvedOccurrence, error: resolutionError } = useQuery({
    queryKey: [
      "recording-exclusion-occurrence",
      userId,
      sessionId,
      event,
      revision,
    ],
    queryFn: async () => {
      const inspection =
        sessionId && !event
          ? await detectCommands.inspectMeetingAccessibility().catch(() => null)
          : null;
      const meeting =
        inspection?.status === "ok"
          ? selectAutomaticMeeting(inspection.data, {
              googleMeet: true,
              slack: true,
            })
          : null;
      return resolveRecordingOccurrence(userId!, { sessionId, event, meeting });
    },
    enabled: Boolean(userId),
    retry: false,
  });
  const occurrence = resolvedOccurrence ?? {
    key: "unidentified",
    room: null,
    kind: "ambiguous" as const,
  };
  let excluded = false;
  let policyError: string | undefined;
  try {
    excluded = Boolean(
      userId &&
      resolvedOccurrence &&
      (isRecordingExcluded(userId, occurrence) ||
        (sessionId && isRecordingSessionExcluded(userId, sessionId))),
    );
  } catch {
    policyError =
      "Saved recording choices could not be read. You can still stop this recording.";
  }
  const action = useMutation({
    mutationFn: async () => {
      if (!userId) return;
      if (excluded) {
        // Never reuse audio or recovery work belonging to an excluded session.
        const nextSessionId = await createSession("Meeting recording");
        resumeRecordingOccurrence(userId, occurrence, nextSessionId);
        useTabs.getState().openNew({
          type: "sessions",
          id: nextSessionId,
          state: { view: null, autoStart: true, scheduledAutoStart: null },
        });
        return;
      }
      const live = listenerStore.getState().live;
      const sameSession = Boolean(sessionId && live.sessionId === sessionId);
      const current =
        !sameSession && live.sessionId
          ? await resolveRecordingOccurrence(userId, {
              sessionId: live.sessionId,
            }).catch(() => null)
          : null;
      const affectsLive = Boolean(
        sameSession ||
        (live.sessionId && !sameSession && !current) ||
        (current &&
          (current.key === occurrence.key ||
            (current.room &&
              current.room === occurrence.room &&
              current.kind !== "calendar"))),
      );
      let boundSessions: string[] = [];
      try {
        boundSessions = getRecordingOccurrenceSessionIds(userId, occurrence);
      } catch {
        /* Native stop remains available if policy storage is unreadable. */
      }
      const sessionIds = [
        ...new Set([
          ...boundSessions,
          ...(sessionId ? [sessionId] : []),
          ...(affectsLive && live.sessionId ? [live.sessionId] : []),
        ]),
      ];
      let persistenceError: unknown;
      try {
        const unidentifiedLive =
          affectsLive &&
          (occurrence.kind === "session" || (!sameSession && !current));
        if (unidentifiedLive)
          excludeRecordingOccurrence(
            userId,
            { key: "unidentified", room: null, kind: "ambiguous" },
            sessionIds,
          );
        excludeRecordingOccurrence(userId, occurrence, sessionIds);
      } catch (error) {
        persistenceError = error;
      }
      const blocking = sessionIds.map((id) =>
        excludeSharedRecording(id, userId),
      );
      if (affectsLive || (sessionId && live.sessionId === sessionId))
        listenerStore.getState().stop();
      const results = await Promise.allSettled(blocking);
      if (persistenceError)
        throw new Error(
          "Recording stopped, but this choice could not be saved. Keep automatic recording off until storage is available.",
        );
      if (results.some((result) => result.status === "rejected"))
        throw new Error(
          "The recording and upload block could not be confirmed. Quit this app to stop capture and contact support before reopening it.",
        );
    },
  });
  if (!userId) return null;
  return (
    <RecordingExclusionView
      excluded={excluded}
      roomHold={occurrence.kind === "room"}
      unidentified={occurrence.kind === "ambiguous" && !occurrence.room}
      busy={action.isPending}
      error={
        action.error?.message ??
        policyError ??
        (resolutionError
          ? "Meeting identity is unavailable. This control will stop your current recorder conservatively."
          : undefined)
      }
      onAction={() => action.mutate()}
    />
  );
}
