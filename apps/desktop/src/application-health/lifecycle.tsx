import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { version as osVersion } from "@tauri-apps/plugin-os";

import { commands as analytics } from "@anlg/plugin-analytics";
import { commands as permissions } from "@anlg/plugin-permissions";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import {
  acknowledgeRecordingDiagnostics,
  configureRecordingDiagnostics,
  recordRecordingDiagnostic,
  recordingDiagnosticBatch,
  untilDiagnosticDeadline,
} from "./recording-diagnostics";

import { useAuth } from "~/auth";
import { env } from "~/env";
import {
  getStoredSettingValues,
  useStoredSettingValuesQuery,
} from "~/settings/queries";
import { sharedRecordingQueue } from "~/shared-recordings/native";
import { resolveConfigValue } from "~/shared/config";
import { useLatestRef } from "~/shared/hooks/useLatestRef";
import { listenerStore } from "~/store/zustand/listener/instance";

export function ApplicationHealthLifecycle() {
  const { session, supabase } = useAuth();
  const settings = useStoredSettingValuesQuery();
  if (
    env.VITE_E3_SCRIBE === "true" &&
    isTauri() &&
    session &&
    settings.data &&
    !settings.error &&
    !resolveConfigValue("telemetry_consent", settings.data)
  )
    return (
      <DisabledRecordingDiagnostics
        key={session.user.id}
        userId={session.user.id}
      />
    );
  if (
    env.VITE_E3_SCRIBE !== "true" ||
    !isTauri() ||
    !session ||
    !supabase ||
    session.user.is_anonymous === true ||
    !settings.data ||
    settings.error ||
    !resolveConfigValue("telemetry_consent", settings.data)
  )
    return null;
  return (
    <ActiveApplicationHealth key={session.user.id} userId={session.user.id} />
  );
}

function DisabledRecordingDiagnostics({ userId }: { userId: string }) {
  useMountEffect(() => {
    void configureRecordingDiagnostics(userId, false, true).catch(() => {});
  });
  return null;
}

function ActiveApplicationHealth({ userId }: { userId: string }) {
  const auth = useLatestRef(useAuth());
  useMountEffect(() => {
    let disposed = false;
    let diagnosticsReady = false;
    let flushing = false;
    let warned = false;
    let healthWarned = false;
    let nativeDeviceId: string | null = null;
    const allowed = async (signal: AbortSignal) => {
      const stored = await getStoredSettingValues();
      const disabled = await analytics.isDisabled();
      signal.throwIfAborted();
      return (
        resolveConfigValue("telemetry_consent", stored) &&
        disabled.status === "ok" &&
        !disabled.data &&
        auth.current.session?.user.id === userId
      );
    };
    let request: AbortController | undefined;
    let diagnosticsRequest: AbortController | undefined;
    const flushDiagnostics = async () => {
      if (disposed || flushing) return;
      flushing = true;
      const current = new AbortController();
      diagnosticsRequest = current;
      const { signal } = current;
      const timeout = setTimeout(() => current.abort(), 10_000);
      const work = async () => {
        if (!(await allowed(signal))) {
          const stored = await getStoredSettingValues();
          const disabled = await analytics.isDisabled();
          if (!disposed)
            await configureRecordingDiagnostics(
              userId,
              false,
              !resolveConfigValue("telemetry_consent", stored) ||
                (disabled.status === "ok" && disabled.data),
            );
          diagnosticsReady = false;
          return;
        }
        if (disposed) return;
        if (!diagnosticsReady) {
          await configureRecordingDiagnostics(userId, true);
          diagnosticsReady = true;
        }
        if (disposed) return;
        const batch = await recordingDiagnosticBatch(userId);
        signal.throwIfAborted();
        nativeDeviceId = batch.deviceId;
        recordRecordingDiagnostic(
          null,
          "policy_decision",
          "collector_started",
          {
            droppedCount: Math.min(batch.droppedCount, 1_000_000_000),
            retentionDropped: batch.retentionDropped,
            osVersion: osVersion().match(/^\d+(?:\.\d+)*/)?.[0],
          },
        );
        if (batch.events.length === 0) return;
        const session = await auth.current.getSessionForRequest();
        if (
          !session ||
          session.user.id !== userId ||
          disposed ||
          !(await allowed(signal))
        )
          return;
        signal.throwIfAborted();
        const { data, error } = await auth.current
          .supabase!.rpc("recording_diagnostics_ingest", {
            p_events: batch.events,
          })
          .setHeader("Authorization", `Bearer ${session.access_token}`)
          .abortSignal(signal);
        if (error) throw error;
        const accepted = Array.isArray(data) ? data : data?.accepted;
        const rejected = Array.isArray(data) ? [] : data?.rejected;
        const sent = new Set(batch.events.map((event) => event.id));
        if (!Array.isArray(accepted) || !Array.isArray(rejected))
          throw new Error("Invalid diagnostic acknowledgement");
        const acknowledged = [...accepted, ...rejected];
        if (acknowledged.some((id) => typeof id !== "string" || !sent.has(id)))
          throw new Error("Invalid diagnostic acknowledgement");
        if (disposed || !(await allowed(signal))) return;
        await acknowledgeRecordingDiagnostics(userId, acknowledged);
        if (rejected.length)
          recordRecordingDiagnostic(
            null,
            "policy_decision",
            "delivery_failed",
            {
              droppedCount: rejected.length,
            },
          );
        warned = false;
      };
      try {
        await untilDiagnosticDeadline(work(), signal);
      } catch {
        if (!disposed && !warned) {
          warned = true;
          console.warn(
            "Recording diagnostics delivery failed; queued events will retry.",
          );
          recordRecordingDiagnostic(null, "policy_decision", "delivery_failed");
        }
      } finally {
        clearTimeout(timeout);
        flushing = false;
      }
    };
    const report = async () => {
      request = new AbortController();
      const current = request;
      const { signal } = current;
      const timeout = setTimeout(() => current.abort(), 10_000);
      try {
        if (!(await allowed(signal))) return null;
        const stored = await getStoredSettingValues();
        const [appVersion, microphone, systemAudio, accessibility, upload] =
          await Promise.all([
            getVersion(),
            permissions.checkPermission("microphone").catch(() => null),
            permissions.checkPermission("systemAudio").catch(() => null),
            permissions.checkPermission("accessibility").catch(() => null),
            readUploadHealth(userId),
          ]);
        signal.throwIfAborted();
        const key = `e3-application-health-device:${userId}`;
        const deviceId =
          nativeDeviceId || localStorage.getItem(key) || crypto.randomUUID();
        localStorage.setItem(key, deviceId);
        const live = listenerStore.getState().live;
        const owned = live.captureOwnerUserId === userId;
        const ownedActive =
          owned && live.captureOwnerSessionId === live.sessionId;
        const snapshot = {
          observedAt: new Date().toISOString(),
          recording:
            live.status === "active" ? (ownedActive ? true : null) : false,
          // Recording history comes from existing uploaded capture metadata.
          lastRecordingStartedAt: null,
          lastRecordingStoppedAt: null,
          lastRecordingDurationSeconds: null,
          errorCategory:
            owned &&
            live.captureOwnerSessionId === live.lastErrorSessionId &&
            live.lastErrorIsAudioRelated &&
            live.lastError
              ? "audio_capture"
              : ownedActive && live.transcriptionStalled
                ? "transcription"
                : null,
          deviceId,
          appVersion,
          ...upload,
          permissions: {
            microphone:
              microphone?.status === "ok" ? microphone.data : "unknown",
            systemAudio:
              systemAudio?.status === "ok" ? systemAudio.data : "unknown",
            accessibility:
              accessibility?.status === "ok" ? accessibility.data : "unknown",
          },
          autoRecordDetected: resolveConfigValue(
            "auto_start_detected_meetings",
            stored,
          ),
          autoRecordScheduled: resolveConfigValue(
            "auto_start_scheduled_meetings",
            stored,
          ),
        };
        const session = await auth.current.getSessionForRequest();
        if (!session || session.user.id !== userId || !(await allowed(signal)))
          return;
        signal.throwIfAborted();
        const { error } = await auth.current
          .supabase!.rpc("application_health_observe", { p_snapshot: snapshot })
          .setHeader("Authorization", `Bearer ${session.access_token}`)
          .abortSignal(signal);
        if (error) throw error;
        healthWarned = false;
      } catch {
        if (!disposed && !healthWarned) {
          healthWarned = true;
          console.warn(
            "Application health report failed; the next heartbeat will retry.",
          );
        }
      } finally {
        clearTimeout(timeout);
      }
    };
    void flushDiagnostics().finally(() => {
      if (!disposed) void report();
    });
    const timer = setInterval(() => void report(), 60_000);
    const diagnosticsTimer = setInterval(() => void flushDiagnostics(), 15_000);
    return () => {
      disposed = true;
      clearInterval(timer);
      clearInterval(diagnosticsTimer);
      request?.abort();
      diagnosticsRequest?.abort();
      void configureRecordingDiagnostics(userId, false).catch(() => {});
      void getStoredSettingValues()
        .then((stored) => {
          if (!resolveConfigValue("telemetry_consent", stored))
            return configureRecordingDiagnostics(userId, false, true);
        })
        .catch(() => {});
    };
  });
  return null;
}

async function readUploadHealth(userId: string) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const queue = await Promise.race([
      sharedRecordingQueue(userId),
      new Promise<null>((resolve) => {
        timer = setTimeout(() => resolve(null), 1_000);
      }),
    ]);
    if (!queue) throw new Error("Upload queue unavailable");
    const pending = queue.filter(
      (job) => job.state !== "uploaded" && !job.state.startsWith("blocked_"),
    );
    return {
      uploadPendingCount: Math.min(pending.length, 4096),
      uploadBlockedCount: Math.min(
        queue.filter((job) => job.state.startsWith("blocked_")).length,
        4096,
      ),
      uploadError: pending.some((job) => job.state === "error"),
    };
  } catch {
    return {
      uploadPendingCount: null,
      uploadBlockedCount: null,
      uploadError: null,
    };
  } finally {
    clearTimeout(timer);
  }
}
