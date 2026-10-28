import { invoke } from "@tauri-apps/api/core";

import {
  excludeRecordingOccurrence,
  isRecordingExcluded,
  isRecordingSessionExcluded,
  type RecordingOccurrence,
} from "./recording-exclusions";

export type NativeMeetingContext = {
  sessionId: string;
  userId: string;
  occurrence: RecordingOccurrence;
  revision: string;
  state: "pending" | "recording" | "excluded";
};

let operations = Promise.resolve();
function serialize<T>(action: () => Promise<T>): Promise<T> {
  const result = operations.then(action);
  operations = result.then(
    () => undefined,
    () => undefined,
  );
  return result;
}

const contexts = new Map<string, NativeMeetingContext>();
const contextKey = (userId: string, sessionId: string) =>
  JSON.stringify([userId, sessionId]);

export function meetingRecordingContext(
  userId: string,
  sessionId: string,
  occurrence: RecordingOccurrence,
): NativeMeetingContext {
  const key = contextKey(userId, sessionId);
  const existing = contexts.get(key);
  if (existing) return existing;
  const context: NativeMeetingContext = {
    sessionId,
    userId,
    occurrence:
      occurrence.kind === "session"
        ? { key: "unidentified", room: null, kind: "ambiguous" }
        : occurrence,
    revision: crypto.randomUUID(),
    state: "pending",
  };
  contexts.set(key, context);
  return context;
}

async function setControl(
  context: NativeMeetingContext,
  resumeOf: string | null = null,
) {
  const result = await invoke<NativeMeetingContext | null>(
    "plugin:transcription|meeting_recording_control_set",
    { context, resumeOf },
  );
  if (
    !result ||
    result.userId !== context.userId ||
    result.sessionId !== context.sessionId ||
    result.revision !== context.revision
  )
    throw new Error(
      "Recording control changed. Reopen the recording menu and try again.",
    );
  contexts.set(contextKey(result.userId, result.sessionId), result);
  return result;
}

export function setMeetingRecordingControl(
  context: NativeMeetingContext,
  resumeOf: string | null = null,
) {
  return serialize(() => setControl(context, resumeOf));
}

export function resumeNativeMeetingRecording(
  context: NativeMeetingContext,
  resumeOf: string | null,
  commitLocal: () => void,
) {
  return serialize(async () => {
    commitLocal();
    return setControl(context, resumeOf);
  });
}

export function replayNativeRecordingExclusions(userId: string) {
  return serialize(async () => {
    const held = await invoke<NativeMeetingContext[]>(
      "plugin:transcription|meeting_recording_exclusions",
      { userId },
    );
    if (!Array.isArray(held))
      throw new Error("Saved recording choices are unavailable");
    const replayedSessions = new Set<string>();
    for (const context of held) {
      if (context.userId !== userId)
        throw new Error("Recording account changed");
      if (
        !isRecordingExcluded(userId, context.occurrence) ||
        !isRecordingSessionExcluded(userId, context.sessionId)
      ) {
        excludeRecordingOccurrence(userId, context.occurrence, [
          context.sessionId,
        ]);
      }
      if (!replayedSessions.has(context.sessionId)) {
        contexts.set(contextKey(userId, context.sessionId), context);
        replayedSessions.add(context.sessionId);
      }
    }
    return held;
  });
}

export async function prepareMeetingRecording(
  userId: string,
  sessionId: string,
  occurrence: RecordingOccurrence,
) {
  const key = contextKey(userId, sessionId);
  const existing = contexts.get(key);
  // A resumed note can open while a different call is now in progress.
  // Bind the pending recorder to that call and invalidate its old menu token.
  if (
    existing &&
    existing.occurrence.key !== occurrence.key &&
    occurrence.kind !== "session"
  )
    contexts.delete(key);
  return setMeetingRecordingControl({
    ...meetingRecordingContext(userId, sessionId, occurrence),
    state: "pending",
  });
}

export async function stopMeetingRecordingForExcludedOccurrence(
  userId: string,
  sessionId: string,
  occurrence: RecordingOccurrence,
) {
  let persistenceError: unknown;
  try {
    excludeRecordingOccurrence(userId, occurrence, [sessionId]);
  } catch (error) {
    persistenceError = error;
  }
  // The running session has crossed into a held call. Block that session while
  // preserving the held call's scope, rather than excluding its previous call.
  await setMeetingRecordingControl({
    userId,
    sessionId,
    occurrence,
    revision: crypto.randomUUID(),
    state: "excluded",
  });
  if (persistenceError)
    throw new Error(
      "Recording stopped, but the saved choice could not be confirmed.",
    );
}
