export type HealthSnapshot = {
  deviceId: string;
  appVersion: string;
  observedAt: string;
  permissions: Record<
    "microphone" | "systemAudio" | "accessibility",
    "authorized" | "denied" | "neverRequested" | "unknown"
  >;
  autoRecordDetected: boolean;
  autoRecordScheduled: boolean;
  uploadPendingCount: number | null;
  uploadBlockedCount: number | null;
  uploadError: boolean | null;
  recording: boolean | null;
  lastRecordingStartedAt: string | null;
  lastRecordingStoppedAt: string | null;
  lastRecordingDurationSeconds: number | null;
  errorCategory: "audio_capture" | "transcription" | null;
};

export function startHealthReporter(input: {
  readLive: () => {
    status: string;
    sessionId: string | null;
    seconds: number;
    lastError: string | null;
    lastErrorIsAudioRelated: boolean;
    transcriptionStalled: boolean;
  };
  subscribe: (changed: () => void) => () => void;
  collect: (
    signal: AbortSignal,
  ) => Promise<Pick<
    HealthSnapshot,
    | "deviceId"
    | "appVersion"
    | "permissions"
    | "autoRecordDetected"
    | "autoRecordScheduled"
    | "uploadPendingCount"
    | "uploadBlockedCount"
    | "uploadError"
  > | null>;
  send: (snapshot: HealthSnapshot, signal: AbortSignal) => Promise<void>;
}) {
  let stopped = false;
  let running = false;
  let lastAttempt = -Infinity;
  let activeId: string | null = null;
  const initial = input.readLive();
  const inheritedId = initial.status === "active" ? initial.sessionId : null;
  let startedAt: string | null = null;
  let stoppedAt: string | null = null;
  let duration: number | null = null;
  let activeSeconds = 0;
  let ownedCaptureSeen = false;
  let previousFingerprint = "";
  let timer: ReturnType<typeof setTimeout>;
  let controller: AbortController | null = null;

  const observe = () => {
    const live = input.readLive();
    const observedId = live.status === "active" ? live.sessionId : null;
    // Keep suppressing the inherited session even if it briefly becomes inactive.
    const nextId = observedId === inheritedId ? null : observedId;
    if (activeId && nextId !== activeId) {
      stoppedAt = new Date().toISOString();
      duration = Math.min(
        Math.max(
          activeSeconds,
          observedId && observedId !== activeId ? 0 : live.seconds,
          0,
        ),
        86400,
      );
    }
    if (nextId && nextId !== activeId) {
      ownedCaptureSeen = true;
      startedAt = new Date(
        Date.now() - Math.max(live.seconds, 0) * 1000,
      ).toISOString();
      activeSeconds = 0;
    }
    activeId = nextId;
    if (activeId) activeSeconds = Math.max(activeSeconds, live.seconds);
    return live;
  };
  const schedule = (delay: number) => {
    clearTimeout(timer);
    if (!stopped) timer = setTimeout(() => void flush(), delay);
  };
  const flush = async () => {
    if (stopped || running) return;
    running = true;
    lastAttempt = Date.now();
    const attempt = new AbortController();
    controller = attempt;
    let deadline: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        (async () => {
          const collected = await input.collect(attempt.signal);
          if (!collected || attempt.signal.aborted || stopped) return;
          const live = observe();
          const inheritedCapture =
            inheritedId &&
            live.status === "active" &&
            live.sessionId === inheritedId;
          await input.send(
            {
              ...collected,
              observedAt: new Date().toISOString(),
              recording: inheritedCapture ? null : activeId !== null,
              lastRecordingStartedAt: startedAt,
              lastRecordingStoppedAt: stoppedAt,
              lastRecordingDurationSeconds: duration,
              errorCategory: inheritedCapture
                ? null
                : live.lastError &&
                    live.lastErrorIsAudioRelated &&
                    (ownedCaptureSeen || live.lastError !== initial.lastError)
                  ? "audio_capture"
                  : live.transcriptionStalled &&
                      (ownedCaptureSeen || !initial.transcriptionStalled)
                    ? "transcription"
                    : null,
            },
            attempt.signal,
          );
        })(),
        new Promise<never>((_, reject) => {
          deadline = setTimeout(() => {
            attempt.abort();
            reject(new Error("Health report timed out"));
          }, 10_000);
        }),
      ]);
    } catch {
      // A latest-state retry replaces history; reporting never gates capture.
    } finally {
      clearTimeout(deadline);
      running = false;
      schedule(60_000);
    }
  };
  const changed = () => {
    const live = observe();
    const fingerprint = JSON.stringify([
      live.status === "active" ? live.sessionId : null,
      !!live.lastError,
      live.transcriptionStalled,
    ]);
    if (fingerprint === previousFingerprint) return;
    previousFingerprint = fingerprint;
    if (!running) schedule(Math.max(0, lastAttempt + 15_000 - Date.now()));
  };
  observe();
  const unsubscribe = input.subscribe(changed);
  schedule(0);
  return () => {
    stopped = true;
    clearTimeout(timer);
    controller?.abort();
    unsubscribe();
  };
}
