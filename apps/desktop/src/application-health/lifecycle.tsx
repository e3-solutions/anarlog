import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";

import { commands as analytics } from "@anlg/plugin-analytics";
import { commands as permissions } from "@anlg/plugin-permissions";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

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

function ActiveApplicationHealth({ userId }: { userId: string }) {
  const auth = useLatestRef(useAuth());
  useMountEffect(() => {
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
        const deviceId = localStorage.getItem(key) || crypto.randomUUID();
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
      } catch {
        // Reporting is best effort; the next heartbeat sends current state.
      } finally {
        clearTimeout(timeout);
      }
    };
    void report();
    const timer = setInterval(() => void report(), 60_000);
    return () => {
      clearInterval(timer);
      request?.abort();
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
