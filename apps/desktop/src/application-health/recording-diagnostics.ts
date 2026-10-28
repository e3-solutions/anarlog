import { invoke, isTauri } from "@tauri-apps/api/core";

import { env } from "~/env";

export type RecordingDiagnosticEvidence = {
  observation?: "active" | "inactive" | "unknown";
  inspectionStatus?: "ok" | "error" | "timeout" | "pending" | "unavailable";
  micStatus?: "ok" | "error" | "timeout" | "pending" | "unavailable";
  micActive?: boolean;
  autoStop?: boolean;
  autoStart?: boolean;
  identityReady?: boolean;
  online?: boolean;
  owned?: boolean;
  speakerRecent?: boolean;
  missingForMs?: number;
  networkHoldRemainingMs?: number;
  probeElapsedMs?: number;
  queuedCount?: number;
  droppedCount?: number;
  retentionDropped?: boolean;
  lastAckAgeMs?: number;
  platform?: "slack" | "googleMeet" | "unknown";
  os?: "macos" | "linux" | "windows" | "unknown";
  leaveControlPresent?: boolean;
  leaveControlEnabled?: boolean;
  leaveControlVisible?: boolean;
  matchedBy?: "context" | "toolbar" | "none";
};

export type RecordingDiagnosticEvent = {
  id: string;
  deviceId: string;
  bootId: string;
  sequence: number;
  observedAtMs: number;
  elapsedMs: number;
  appVersion: string;
  buildSha: string;
  sessionId: string | null;
  kind: string;
  reason: string;
  evidence: RecordingDiagnosticEvidence;
};

export type RecordingDiagnosticBatch = {
  deviceId: string;
  bootId: string;
  events: RecordingDiagnosticEvent[];
  droppedCount: number;
  retentionDropped: boolean;
  lastAckAtMs: number | null;
  queuedCount: number;
};

let owner: string | null = null;
let desiredOwner: string | null = null;
let generation = 0;
let configureQueue = Promise.resolve();
const pendingBatches = new Map<string, Promise<RecordingDiagnosticBatch>>();
const recent = new Map<string, { signature: string; at: number }>();
const command = (name: string) =>
  `plugin:transcription|recording_diagnostics_${name}`;

export function configureRecordingDiagnostics(
  userId: string,
  enabled: boolean,
  purge = false,
) {
  if (enabled && desiredOwner === userId) return configureQueue;
  const changesOwner = enabled || desiredOwner === userId;
  const version = changesOwner ? ++generation : generation;
  if (changesOwner) {
    desiredOwner = enabled ? userId : null;
    owner = null;
    recent.clear();
  }
  configureQueue = configureQueue
    .catch(() => {})
    .then(async () => {
      await invoke<void>(command("configure"), { userId, enabled, purge });
      if (version === generation && enabled && desiredOwner === userId)
        owner = userId;
    })
    .catch((error) => {
      if (version === generation && changesOwner) {
        desiredOwner = null;
        owner = null;
      }
      throw error;
    });
  return configureQueue;
}

export function recordRecordingDiagnostic(
  sessionId: string | null,
  kind: string,
  reason: string,
  evidence: RecordingDiagnosticEvidence = {},
) {
  if (!owner || env.VITE_E3_SCRIBE !== "true" || !isTauri()) return;
  const key = `${sessionId}:${kind}${kind === "detector_observation" ? `:${reason}` : ""}`;
  const { missingForMs, networkHoldRemainingMs, probeElapsedMs, ...stable } =
    evidence;
  const signature = JSON.stringify({ reason, evidence: stable });
  const now = Date.now();
  const previous = recent.get(key);
  if (previous?.signature === signature && now - previous.at < 30_000) return;
  if (recent.size >= 64 && !recent.has(key))
    recent.delete(recent.keys().next().value!);
  recent.set(key, { signature, at: now });
  const bounded = (value: number | undefined) =>
    value === undefined
      ? undefined
      : Math.min(86_400_000, Math.max(0, Math.round(value)));
  void invoke<void>(command("record"), {
    userId: owner,
    input: {
      sessionId,
      kind,
      reason,
      evidence: {
        ...stable,
        missingForMs: bounded(missingForMs),
        networkHoldRemainingMs: bounded(networkHoldRemainingMs),
        probeElapsedMs: bounded(probeElapsedMs),
      },
    },
  }).catch(() => {
    recent.delete(key);
  });
}

export function recordingDiagnosticBatch(userId: string) {
  const pending = pendingBatches.get(userId);
  if (pending) return pending;
  const request = invoke<RecordingDiagnosticBatch>(command("batch"), {
    userId,
  }).finally(() => pendingBatches.delete(userId));
  pendingBatches.set(userId, request);
  return request;
}

export function acknowledgeRecordingDiagnostics(
  userId: string,
  eventIds: string[],
) {
  return invoke<void>(command("ack"), { userId, eventIds });
}

export function untilDiagnosticDeadline<T>(
  request: Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  signal.throwIfAborted();
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(signal.reason);
    signal.addEventListener("abort", abort, { once: true });
    request
      .then(resolve, reject)
      .finally(() => signal.removeEventListener("abort", abort));
  });
}
