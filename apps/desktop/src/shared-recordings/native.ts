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
    | "blocked_timeline";
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
  platform: "googleMeet" | "slack";
  callIdentity: string | null;
  observedAtMs: number;
  accessToken: string;
  userId: string;
}) {
  if (!sharedRecordingsPreviewUrl) return Promise.resolve();
  return invoke<unknown>(command("observe"), input);
}

export async function downloadSharedRecordingAudio(input: {
  userId: string;
  organizationId: string;
  occurrenceId: string;
  revision: string;
  audioSha256: string;
  accessToken: string;
}) {
  const path = await invoke<string>(command("download"), {
    ...input,
    apiBaseUrl: sharedRecordingsPreviewUrl,
  });
  return convertFileSrc(path);
}
