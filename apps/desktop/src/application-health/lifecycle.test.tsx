import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  native: true,
  userId: "account-a" as string | null,
  anonymous: false,
  consent: true,
  disabled: false,
  settingsReady: true,
  permission: vi.fn(),
  version: vi.fn(),
  requestSession: vi.fn(),
  queue: vi.fn(),
  fetch: vi.fn(),
}));
const stored = () => ({
  values: {
    telemetry_consent: state.consent,
    auto_start_detected_meetings: true,
    auto_start_scheduled_meetings: false,
  },
  hasValues: new Set([
    "telemetry_consent",
    "auto_start_detected_meetings",
    "auto_start_scheduled_meetings",
  ]),
});
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => state.native }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: state.version }));
vi.mock("@anlg/plugin-analytics", () => ({
  commands: {
    isDisabled: async () => ({ status: "ok", data: state.disabled }),
  },
}));
vi.mock("@anlg/plugin-permissions", () => ({
  commands: { checkPermission: state.permission },
}));
vi.mock("~/env", () => ({
  env: { VITE_E3_SCRIBE: "true", VITE_API_URL: "https://fixture.invalid/" },
}));
vi.mock("~/auth", () => ({
  useAuth: () => ({
    session: state.userId
      ? { user: { id: state.userId, is_anonymous: state.anonymous } }
      : null,
    getSessionForRequest: state.requestSession,
  }),
}));
vi.mock("~/settings/queries", () => ({
  useStoredSettingValuesQuery: () => ({
    data: state.settingsReady ? stored() : undefined,
    error: null,
  }),
  getStoredSettingValues: async () => stored(),
}));
vi.mock("~/shared-recordings/native", () => ({
  sharedRecordingQueue: state.queue,
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: {
    getState: () => ({
      live: {
        status: "inactive",
        sessionId: null,
        seconds: 0,
        lastError: "secret raw message",
        lastErrorIsAudioRelated: true,
        transcriptionStalled: false,
      },
    }),
    subscribe: () => () => {},
  },
}));

import { ApplicationHealthLifecycle } from "./lifecycle";

beforeEach(() => {
  vi.useFakeTimers();
  state.native = true;
  state.userId = "account-a";
  state.anonymous = false;
  state.consent = true;
  state.disabled = false;
  state.settingsReady = true;
  state.queue.mockResolvedValue([]);
  state.version.mockResolvedValue("0.1.21");
  state.permission.mockResolvedValue({ status: "ok", data: "authorized" });
  state.requestSession.mockImplementation(async () => ({
    user: { id: state.userId },
    access_token: "fixture-token",
  }));
  state.fetch.mockResolvedValue({ ok: true });
  vi.stubGlobal("fetch", state.fetch);
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
const tick = async () => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1);
  });
};

it("reports authenticated native launches with bounded operational metadata and partial permission readiness", async () => {
  state.permission.mockImplementation(async (permission: string) => {
    if (permission === "accessibility")
      throw new Error("private native exception");
    return {
      status: "ok",
      data: permission === "microphone" ? "authorized" : "denied",
    };
  });
  const first = render(<ApplicationHealthLifecycle />);
  await tick();
  const [url, request] = state.fetch.mock.calls[0];
  expect(url).toBe("https://fixture.invalid/v1/application-health");
  expect(request.headers.Authorization).toBe("Bearer fixture-token");
  const body = JSON.parse(request.body);
  expect(body).toEqual({
    deviceId: expect.any(String),
    appVersion: "0.1.21",
    observedAt: expect.any(String),
    permissions: {
      microphone: "authorized",
      systemAudio: "denied",
      accessibility: "unknown",
    },
    autoRecordDetected: true,
    autoRecordScheduled: false,
    recording: false,
    lastRecordingStartedAt: null,
    lastRecordingStoppedAt: null,
    lastRecordingDurationSeconds: null,
    errorCategory: null,
    uploadPendingCount: 0,
    uploadBlockedCount: 0,
    uploadError: false,
  });
  first.unmount();
  render(<ApplicationHealthLifecycle />);
  await tick();
  expect(JSON.parse(state.fetch.mock.calls[1][1].body).deviceId).toBe(
    body.deviceId,
  );
});

it("reports account-scoped upload failures and blocked jobs without exposing source data, and keeps unavailable queues unknown", async () => {
  for (const [read, expected] of [
    [
      async () => [
        { state: "uploaded", title: "private uploaded title" },
        { state: "pending", path: "/private/source.wav" },
        { state: "error", lastError: "private raw upload error" },
        { state: "blocked_unverified", meetingUrl: "https://private.invalid" },
      ],
      { uploadPendingCount: 2, uploadBlockedCount: 1, uploadError: true },
    ],
    [
      async () => {
        throw new Error("private native exception");
      },
      { uploadPendingCount: null, uploadBlockedCount: null, uploadError: null },
    ],
    [
      () => new Promise(() => {}),
      { uploadPendingCount: null, uploadBlockedCount: null, uploadError: null },
    ],
  ] as const) {
    state.queue.mockImplementation(read);
    state.fetch.mockClear();
    const view = render(<ApplicationHealthLifecycle />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_001);
    });
    const payload = state.fetch.mock.calls[0][1].body;
    expect(JSON.parse(payload)).toMatchObject(expected);
    expect(payload).not.toContain("private");
    expect(state.queue).toHaveBeenLastCalledWith("account-a");
    view.unmount();
  }
});

it("does not confuse browser signup, anonymous sessions, unavailable settings, or opted-out installs with native launches", async () => {
  for (const change of [
    () => {
      state.native = false;
    },
    () => {
      state.userId = null;
    },
    () => {
      state.anonymous = true;
    },
    () => {
      state.settingsReady = false;
    },
    () => {
      state.consent = false;
    },
    () => {
      state.disabled = true;
    },
  ]) {
    state.native = true;
    state.userId = "account-a";
    state.anonymous = false;
    state.settingsReady = true;
    state.consent = true;
    state.disabled = false;
    change();
    const view = render(<ApplicationHealthLifecycle />);
    await tick();
    expect(state.fetch.mock.calls).toHaveLength(0);
    view.unmount();
  }
});

it("drops in-flight observations after consent revocation or account changes and isolates device identities", async () => {
  let finishVersion: (version: string) => void = () => {};
  state.version.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        finishVersion = resolve;
      }),
  );
  const view = render(<ApplicationHealthLifecycle />);
  await tick();
  state.consent = false;
  finishVersion("0.1.21");
  await tick();
  expect(state.fetch.mock.calls).toHaveLength(0);
  state.consent = true;
  view.rerender(<ApplicationHealthLifecycle />);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  const firstId = JSON.parse(state.fetch.mock.calls[0][1].body).deviceId;
  state.requestSession.mockResolvedValueOnce({
    user: { id: "account-a" },
    access_token: "old-token",
  });
  state.userId = "account-b";
  view.rerender(<ApplicationHealthLifecycle />);
  await tick();
  expect(state.fetch.mock.calls).toHaveLength(1);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  expect(JSON.parse(state.fetch.mock.calls[1][1].body).deviceId).not.toBe(
    firstId,
  );
});
