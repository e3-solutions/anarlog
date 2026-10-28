import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  native: true,
  userId: "account-a" as string | null,
  consent: true,
  permission: vi.fn(),
  version: vi.fn(),
  requestSession: vi.fn(),
  queue: vi.fn(),
  rpc: vi.fn(),
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
    isDisabled: async () => ({ status: "ok", data: false }),
  },
}));
vi.mock("@anlg/plugin-permissions", () => ({
  commands: { checkPermission: state.permission },
}));
vi.mock("~/env", () => ({
  env: { VITE_E3_SCRIBE: "true" },
}));
vi.mock("~/auth", () => ({
  useAuth: () => ({
    session: state.userId
      ? { user: { id: state.userId, is_anonymous: false } }
      : null,
    getSessionForRequest: state.requestSession,
    supabase: { rpc: state.rpc },
  }),
}));
vi.mock("~/settings/queries", () => ({
  useStoredSettingValuesQuery: () => ({
    data: stored(),
    error: null,
  }),
  getStoredSettingValues: async () => stored(),
}));
vi.mock("~/shared-recordings/native", () => ({
  sharedRecordingQueue: state.queue,
}));
vi.mock("~/store/zustand/listener/instance", async () => {
  const { initialGeneralState } =
    await import("~/store/zustand/listener/general-shared");
  return { listenerStore: { getState: () => initialGeneralState } };
});

import { ApplicationHealthLifecycle } from "./lifecycle";

beforeEach(() => {
  vi.useFakeTimers();
  state.native = true;
  state.userId = "account-a";
  state.consent = true;
  state.queue.mockResolvedValue([]);
  state.version.mockResolvedValue("0.1.21");
  state.permission.mockResolvedValue({ status: "ok", data: "authorized" });
  state.requestSession.mockImplementation(async () => ({
    user: { id: state.userId },
    access_token: "fixture-token",
  }));
  state.rpc.mockImplementation(() => ({
    setHeader: () => ({ abortSignal: async () => ({ error: null }) }),
  }));
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
});
const tick = async () => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1);
  });
};

it("sends only operational fields with unknown permissions on native failure", async () => {
  state.permission.mockRejectedValue(new Error("private native exception"));
  state.queue.mockResolvedValue([
    { state: "uploaded", title: "private title" },
    { state: "error", lastError: "private error" },
    { state: "blocked_unverified", meetingUrl: "private url" },
  ]);
  const view = render(<ApplicationHealthLifecycle />);
  await tick();
  const [name, { p_snapshot: snapshot }] = state.rpc.mock.calls[0];
  expect(name).toBe("application_health_observe");
  expect(snapshot).toMatchObject({
    appVersion: "0.1.21",
    permissions: {
      microphone: "unknown",
      systemAudio: "unknown",
      accessibility: "unknown",
    },
    autoRecordDetected: true,
    autoRecordScheduled: false,
    uploadPendingCount: 1,
    uploadBlockedCount: 1,
    uploadError: true,
    errorCategory: null,
  });
  expect(JSON.stringify(snapshot)).not.toContain("private");
  view.unmount();
});

it("drops in-flight observations after consent revocation or account changes and isolates device identities", async () => {
  state.native = false;
  const browser = render(<ApplicationHealthLifecycle />);
  await tick();
  expect(state.rpc).not.toHaveBeenCalled();
  browser.unmount();
  state.native = true;
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
  expect(state.rpc.mock.calls).toHaveLength(0);
  state.consent = true;
  view.rerender(<ApplicationHealthLifecycle />);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  const firstId = state.rpc.mock.calls[0][1].p_snapshot.deviceId;
  state.requestSession.mockResolvedValueOnce({
    user: { id: "account-a" },
    access_token: "old-token",
  });
  state.userId = "account-b";
  view.rerender(<ApplicationHealthLifecycle />);
  await tick();
  expect(state.rpc.mock.calls).toHaveLength(1);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  expect(state.rpc.mock.calls[1][1].p_snapshot.deviceId).not.toBe(firstId);
});
