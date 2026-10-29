import { useMutation, useQuery } from "@tanstack/react-query";
import { useRef, useSyncExternalStore } from "react";

import { commands as detectCommands } from "@anlg/plugin-detect";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import { selectAutomaticMeeting } from "./automatic-meeting";
import {
  excludeMeetingRecording,
  resumeMeetingRecording,
} from "./recording-exclusion-actions";
import { RecordingExclusionView } from "./recording-exclusion-view";

import { useOptionalAuth } from "~/auth";
import {
  getRecordingExclusionsSnapshot,
  isRecordingExcluded,
  isRecordingSessionExcluded,
  resolveRecordingOccurrence,
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
  const identity = useRef(userId);
  identity.current = userId;
  useMountEffect(() => {
    identity.current = userId;
    return () => {
      identity.current = undefined;
    };
  });
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
      const input = {
        userId,
        occurrence,
        sessionId,
        isCurrent: () => identity.current === userId,
      };
      if (excluded) await resumeMeetingRecording(input);
      else await excludeMeetingRecording(input);
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
