import { invoke } from "@tauri-apps/api/core";

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
  diagnosticSummary?: {
    inspectionStatus: "ok" | "error" | "unavailable";
    rowCount: number;
    activeCallCount: number;
    trustedCount: number;
    warningCategories: string[];
  };
}) {
  if (!sharedRecordingsPreviewUrl) return Promise.resolve(null);
  return invoke<string | null>(command("observe"), input);
}
