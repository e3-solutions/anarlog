import { afterEach, expect, it, vi } from "vitest";

import { startHealthReporter, type HealthSnapshot } from "./reporter";

afterEach(() => vi.useRealTimers());

it.each([false, true])(
  "does not attribute a delayed previous-account capture when mounted with pending start=%s",
  async (pendingStart) => {
    vi.useFakeTimers();
    let live = {
      status: "inactive",
      sessionId: pendingStart ? "capture-a" : (null as string | null),
      captureOwnerUserId: pendingStart ? "account-a" : (null as string | null),
      captureOwnerSessionId: pendingStart
        ? "capture-a"
        : (null as string | null),
      seconds: 0,
      lastError: null as string | null,
      lastErrorSessionId: null as string | null,
      lastErrorIsAudioRelated: false,
      transcriptionStalled: false,
    };
    let changed = () => {};
    const reports: HealthSnapshot[] = [];
    const stop = startHealthReporter({
      userId: "account-b",
      readLive: () => live,
      subscribe: (callback) => {
        changed = callback;
        return () => {};
      },
      collect: async () => ({
        deviceId: "11111111-1111-4111-8111-111111111111",
        appVersion: "0.1.21",
        permissions: {
          microphone: "authorized",
          systemAudio: "authorized",
          accessibility: "authorized",
        },
        autoRecordDetected: true,
        autoRecordScheduled: true,
        uploadPendingCount: 0,
        uploadBlockedCount: 0,
        uploadError: false,
      }),
      send: async (snapshot) => {
        reports.push(snapshot);
      },
    });
    const update = async (next: Partial<typeof live>) => {
      live = { ...live, ...next };
      changed();
      await vi.advanceTimersByTimeAsync(60_000);
      return reports[reports.length - 1]!;
    };
    await vi.advanceTimersByTimeAsync(0);
    let result = await update({
      status: "active",
      sessionId: "capture-a",
      captureOwnerUserId: "account-a",
      captureOwnerSessionId: "capture-a",
      seconds: 8,
    });
    expect(result).toMatchObject({
      recording: null,
      lastRecordingStartedAt: null,
      lastRecordingStoppedAt: null,
      lastRecordingDurationSeconds: null,
      errorCategory: null,
    });
    result = await update({
      status: "inactive",
      lastError: "private previous account capture failed",
      lastErrorSessionId: "capture-a",
      lastErrorIsAudioRelated: true,
    });
    expect(result).toMatchObject({
      recording: false,
      lastRecordingStartedAt: null,
      lastRecordingStoppedAt: null,
      lastRecordingDurationSeconds: null,
      errorCategory: null,
    });
    result = await update({
      status: "active",
      sessionId: "native-unknown",
      captureOwnerUserId: "account-b",
      captureOwnerSessionId: "stale-other-capture",
      seconds: 12,
    });
    expect(result).toMatchObject({
      recording: null,
      lastRecordingStartedAt: null,
      errorCategory: null,
    });
    result = await update({
      status: "active",
      sessionId: "capture-b",
      captureOwnerUserId: "account-b",
      captureOwnerSessionId: "capture-b",
      seconds: 0,
      lastError: null,
      lastErrorSessionId: null,
      lastErrorIsAudioRelated: false,
    });
    expect(result).toMatchObject({
      recording: true,
      lastRecordingStartedAt: expect.any(String),
      lastRecordingStoppedAt: null,
    });
    await update({ seconds: 7 });
    result = await update({ status: "inactive", sessionId: null });
    expect(result).toMatchObject({
      recording: false,
      lastRecordingStoppedAt: expect.any(String),
      lastRecordingDurationSeconds: 7,
      errorCategory: null,
    });
    expect(JSON.stringify(reports)).not.toContain("private previous account");
    stop();
  },
);
