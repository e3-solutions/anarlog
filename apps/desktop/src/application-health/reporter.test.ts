import { afterEach, expect, it, vi } from "vitest";

import { startHealthReporter, type HealthSnapshot } from "./reporter";

afterEach(() => vi.useRealTimers());

function setup(initialActive = false) {
  vi.useFakeTimers();
  let live = {
    status: initialActive ? "active" : "inactive",
    sessionId: initialActive
      ? "previous-account-session"
      : (null as string | null),
    seconds: 0,
    captureOwnerUserId: initialActive ? "previous-account" : "current-account",
    captureOwnerSessionId: initialActive
      ? "previous-account-session"
      : (null as string | null),
    lastError: null as string | null,
    lastErrorSessionId: null as string | null,
    lastErrorIsAudioRelated: false,
    transcriptionStalled: false,
  };
  let changed = () => {};
  const reports: HealthSnapshot[] = [];
  let offline = false;
  const collect = vi.fn(async () => ({
    deviceId: "d4c68dd1-7ec4-4986-bc61-f40445764958",
    appVersion: "0.1.20",
    permissions: {
      microphone: "authorized",
      systemAudio: "denied",
      accessibility: "unknown",
    } as const,
    autoRecordDetected: false,
    autoRecordScheduled: true,
    uploadPendingCount: 0,
    uploadBlockedCount: 0,
    uploadError: false,
  }));
  const stop = startHealthReporter({
    userId: "current-account",
    readLive: () => live,
    subscribe: (callback) => {
      changed = callback;
      return () => {
        changed = () => {};
      };
    },
    collect,
    send: async (snapshot) => {
      if (offline) throw Error("offline");
      reports.push(snapshot);
    },
  });
  return {
    reports,
    collect,
    stop,
    offline: (value: boolean) => {
      offline = value;
    },
    update: (values: Partial<typeof live>) => {
      live = { ...live, ...values };
      changed();
    },
  };
}

it("retains a short recording start and stop across offline retries without queuing history", async () => {
  const run = setup();
  run.offline(true);
  await vi.advanceTimersByTimeAsync(0);
  run.update({
    status: "active",
    sessionId: "private-local-id",
    captureOwnerSessionId: "private-local-id",
  });
  run.update({ seconds: 4 });
  run.update({ status: "inactive", sessionId: null });
  await vi.advanceTimersByTimeAsync(15_000);
  run.offline(false);
  await vi.advanceTimersByTimeAsync(60_000);
  expect(run.reports).toHaveLength(1);
  expect(run.reports[0]).toMatchObject({
    recording: false,
    lastRecordingDurationSeconds: 4,
  });
  expect(run.reports[0].lastRecordingStartedAt).not.toBeNull();
  expect(run.reports[0].lastRecordingStoppedAt).not.toBeNull();
  run.stop();
});

it("reports inherited active capture as unknown until a new account-owned capture starts", async () => {
  const run = setup(true);
  await vi.advanceTimersByTimeAsync(0);
  expect(run.reports[0]).toMatchObject({
    recording: null,
    lastRecordingStartedAt: null,
  });
  run.update({ seconds: 45 });
  run.update({ status: "inactive", sessionId: null });
  await vi.advanceTimersByTimeAsync(15_000);
  expect(run.reports[run.reports.length - 1]).toMatchObject({
    recording: false,
    lastRecordingStoppedAt: null,
  });
  run.update({
    status: "active",
    sessionId: "previous-account-session",
    seconds: 46,
  });
  await vi.advanceTimersByTimeAsync(15_000);
  expect(run.reports[run.reports.length - 1]).toMatchObject({
    recording: null,
    lastRecordingStartedAt: null,
    lastRecordingStoppedAt: null,
    lastRecordingDurationSeconds: null,
  });
  run.update({
    status: "active",
    sessionId: "current-account-session",
    captureOwnerUserId: "current-account",
    captureOwnerSessionId: "current-account-session",
    seconds: 0,
  });
  await vi.advanceTimersByTimeAsync(15_000);
  expect(run.reports[run.reports.length - 1]?.recording).toBe(true);
  run.stop();
});

it("aborts a pending collection and prevents late account data from being sent after cleanup", async () => {
  const run = setup();
  let release: (() => void) | undefined;
  const original = run.collect.getMockImplementation()!;
  run.collect.mockImplementation(async () => {
    await new Promise<void>((resolve) => {
      release = resolve;
    });
    return original();
  });
  await vi.advanceTimersByTimeAsync(0);
  run.stop();
  release?.();
  await vi.advanceTimersByTimeAsync(120_000);
  expect(run.reports).toEqual([]);
});

it("recovers from a hung native collector and bounds repeated listener updates", async () => {
  const run = setup();
  run.collect.mockImplementationOnce(() => new Promise(() => {}));
  await vi.advanceTimersByTimeAsync(10_000);
  await vi.advanceTimersByTimeAsync(60_000);
  expect(run.reports).toHaveLength(1);
  for (let seconds = 1; seconds < 100; seconds++) run.update({ seconds });
  await vi.advanceTimersByTimeAsync(14_999);
  expect(run.reports).toHaveLength(1);
  run.stop();
});
