import { convertFileSrc, invoke } from "@tauri-apps/api/core";

import {
  sharedRecordingsPreviewUrl,
  type SharedRecordingAccount,
} from "./client";

export type SharedRecordingConfiguration = {
  enabled: boolean;
  apiBaseUrl: string;
  organizationId: string;
  userId: string;
  connections: SharedRecordingAccount["connections"];
};

export type SharedRecordingJob = {
  id: string;
  sessionId: string;
  state:
    | "preparing"
    | "awaiting_identity"
    | "pending"
    | "uploaded"
    | "error"
    | "blocked_storage"
    | "blocked_timeline"
    | "blocked_identity";
  error: string | null;
};

const command = (name: string) =>
  `plugin:transcription|shared_recording_${name}`;

export function sharedRecordingConfiguration() {
  return invoke<SharedRecordingConfiguration | null>(command("configuration"));
}

export function configureSharedRecordings(
  config: SharedRecordingConfiguration,
) {
  return invoke<SharedRecordingConfiguration>(command("configure"), { config });
}

export function sharedRecordingQueue(userId: string) {
  return invoke<SharedRecordingJob[]>(command("queue"), { userId });
}

export function flushSharedRecordings(accessToken: string, userId: string) {
  return invoke<void>(command("flush"), { accessToken, userId });
}

export function observeSharedRecording(input: {
  sessionId: string;
  platform: "googleMeet" | "slack" | "unverified";
  callIdentity: string | null;
  observedAtMs: number;
  accessToken: string;
  userId: string;
}) {
  if (!sharedRecordingsPreviewUrl) return Promise.resolve();
  return invoke<unknown>(command("observe"), input);
}

export async function downloadSharedRecordingAudio(
  input: {
    userId: string;
    organizationId: string;
    occurrenceId: string;
    revision: string;
    audioSha256: string;
    accessToken: string;
  },
  signal: AbortSignal,
) {
  signal.throwIfAborted();
  const path = await invoke<string>(command("download"), {
    ...input,
    apiBaseUrl: sharedRecordingsPreviewUrl,
  });
  signal.throwIfAborted();
  const response = await fetch(convertFileSrc(path), { signal });
  if (!response.ok || !response.body)
    throw new Error(
      "Shared audio could not be loaded from its verified cache.",
    );
  const limit = 512 * 1024 * 1024;
  if (Number(response.headers.get("content-length")) > limit) {
    await response.body.cancel();
    throw new Error("Shared audio exceeds the playback size limit.");
  }
  const reader = response.body.getReader();
  const chunks: BlobPart[] = [];
  let length = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) {
        await reader.cancel();
        throw new Error("Shared audio exceeds the playback size limit.");
      }
      chunks.push(new Uint8Array(value));
    }
  } finally {
    reader.releaseLock();
  }
  signal.throwIfAborted();
  if (length === 0) throw new Error("Shared audio is empty.");
  return new Blob(chunks, { type: "audio/wav" });
}
