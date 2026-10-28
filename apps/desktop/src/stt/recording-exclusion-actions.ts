import {
  meetingRecordingContext,
  replayNativeRecordingExclusions,
  resumeNativeMeetingRecording,
  setMeetingRecordingControl,
  type NativeMeetingContext,
} from "./meeting-recording-control";
import {
  excludeRecordingOccurrence,
  getRecordingOccurrenceSessionIds,
  resolveRecordingOccurrence,
  resumeRecordingOccurrence,
  type RecordingOccurrence,
} from "./recording-exclusions";

import { createSession } from "~/session/queries";
import { excludeSharedRecording } from "~/shared-recordings/native";
import { listenerStore } from "~/store/zustand/listener/instance";
import { useTabs } from "~/store/zustand/tabs";

export async function excludeMeetingRecording({
  userId,
  occurrence,
  sessionId,
  isCurrent,
}: {
  userId: string;
  occurrence: RecordingOccurrence;
  sessionId?: string;
  isCurrent: () => boolean;
}) {
  if (!isCurrent()) throw new Error("Recording account changed");
  const live = listenerStore.getState().live;
  const sameSession = Boolean(sessionId && live.sessionId === sessionId);
  const current =
    !sameSession && live.sessionId
      ? await resolveRecordingOccurrence(userId, {
          sessionId: live.sessionId,
        }).catch(() => null)
      : null;
  if (!isCurrent()) throw new Error("Recording account changed");
  const affectsLive = Boolean(
    sameSession ||
    (live.sessionId && !current) ||
    (current &&
      (current.key === occurrence.key ||
        (current.room &&
          current.room === occurrence.room &&
          current.kind !== "calendar"))),
  );
  let bound: string[] = [];
  try {
    bound = getRecordingOccurrenceSessionIds(userId, occurrence);
  } catch {
    /* Keep native stop available. */
  }
  const sessionIds = [
    ...new Set([
      ...bound,
      ...(sessionId ? [sessionId] : []),
      ...(affectsLive && live.sessionId ? [live.sessionId] : []),
    ]),
  ];
  const effective =
    affectsLive && (occurrence.kind === "session" || (!sameSession && !current))
      ? { key: "unidentified", room: null, kind: "ambiguous" as const }
      : occurrence;
  let persistenceError: unknown;
  try {
    excludeRecordingOccurrence(userId, effective, sessionIds);
    if (effective !== occurrence)
      excludeRecordingOccurrence(userId, occurrence, sessionIds);
  } catch (error) {
    persistenceError = error;
  }
  const results = await Promise.allSettled(
    sessionIds.map((id) =>
      affectsLive && id === live.sessionId
        ? setMeetingRecordingControl({
            ...meetingRecordingContext(userId, id, effective),
            state: "excluded",
          })
        : excludeSharedRecording(id, userId),
    ),
  );
  const latest = listenerStore.getState().live;
  if (isCurrent() && affectsLive && latest.sessionId === live.sessionId)
    listenerStore.getState().stop();
  if (results.some((result) => result.status === "rejected"))
    throw new Error(
      "Recording stop could not be confirmed. Quit this app to stop capture before trying again.",
    );
  if (persistenceError)
    throw new Error(
      "Recording stopped, but the saved choice could not be confirmed. Keep recording off until storage is available.",
    );
}

let resuming = false;
export async function resumeMeetingRecording({
  userId,
  occurrence,
  isCurrent,
  expected,
}: {
  userId: string;
  occurrence: RecordingOccurrence;
  isCurrent: () => boolean;
  expected?: NativeMeetingContext;
}) {
  if (resuming) return;
  resuming = true;
  let releasedLocally = false;
  let nextSessionId: string | undefined;
  let previousHolds: NativeMeetingContext[] = [];
  const assertCurrent = () => {
    const live = listenerStore.getState().live;
    if (!isCurrent()) throw new Error("Recording account changed");
    if (
      live.loading ||
      live.status === "active" ||
      live.status === "finalizing"
    )
      throw new Error(
        "Finish the current recording before resuming this meeting.",
      );
  };
  try {
    assertCurrent();
    const holds = await replayNativeRecordingExclusions(userId);
    previousHolds = holds;
    assertCurrent();
    const held = expected
      ? holds.find(
          (item) =>
            item.revision === expected.revision &&
            item.sessionId === expected.sessionId,
        )
      : holds.find(
          (item) =>
            item.occurrence.key === occurrence.key ||
            (item.occurrence.kind === "ambiguous" && !item.occurrence.room) ||
            (item.occurrence.kind !== "calendar" &&
              !!occurrence.room &&
              item.occurrence.room === occurrence.room),
        );
    if (expected && !held)
      throw new Error(
        "This recording menu is out of date. Reopen it to continue.",
      );
    nextSessionId = await createSession("Meeting recording");
    assertCurrent();
    const fresh = nextSessionId;
    await resumeNativeMeetingRecording(
      meetingRecordingContext(userId, fresh, occurrence),
      held?.revision ?? null,
      () => {
        assertCurrent();
        resumeRecordingOccurrence(userId, occurrence, fresh);
        releasedLocally = true;
      },
    );
    assertCurrent();
    useTabs.getState().openNew({
      type: "sessions",
      id: nextSessionId,
      state: { view: null, autoStart: true, scheduledAutoStart: null },
    });
  } catch (error) {
    if (releasedLocally) {
      // Failed native acknowledgement must never leave a relaxed frontend policy.
      for (const held of previousHolds) {
        excludeRecordingOccurrence(userId, held.occurrence, [held.sessionId]);
      }
      excludeRecordingOccurrence(
        userId,
        occurrence,
        nextSessionId ? [nextSessionId] : [],
      );
    }
    throw error;
  } finally {
    resuming = false;
  }
}
