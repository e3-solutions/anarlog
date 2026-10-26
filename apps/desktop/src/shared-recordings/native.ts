import { invoke } from "@tauri-apps/api/core";

import type {
  Configuration,
  Job,
  ObservationSummary,
} from "@anlg/plugin-transcription";

import { sharedRecordingsPreviewUrl } from "./client";

export type SharedRecordingConfiguration = Configuration;
export type SharedRecordingJob = Pick<
  Job,
  "id" | "sessionId" | "state" | "error"
>;

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
  quarantineReason?:
    | "ax_unavailable"
    | "ax_ambiguous"
    | "meeting_unverified"
    | "inventory_missing"
    | "inventory_conflict"
    | "meeting_departed"
    | "call_changed"
    | "auth_unavailable"
    | "observer_stopped";
  diagnosticSummary?: ObservationSummary;
}) {
  if (!sharedRecordingsPreviewUrl) return Promise.resolve(null);
  return invoke<string | null>(command("observe"), input);
}
