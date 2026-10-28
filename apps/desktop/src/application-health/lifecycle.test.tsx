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
  invoke: vi.fn(),
  events: [] as Array<Record<string, unknown>>,
  received: [] as Array<Record<string, unknown>>,
  offline: false,
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
vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => state.native,
  invoke: state.invoke,
}));
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
  state.offline = false;
  state.events = [];
  state.received = [];
  state.invoke.mockImplementation(
    async (
      command: string,
      args: {
        eventIds?: string[];
        enabled?: boolean;
        purge?: boolean;
        userId?: string;
      },
    ) => {
      if (command.endsWith("_batch"))
        return {
          deviceId: `fixture-device:${args.userId}`,
          bootId: "fixture-boot",
          events: [...state.events],
          droppedCount: 0,
          retentionDropped: false,
        };
      if (command.endsWith("_ack"))
        state.events = state.events.filter(
          (event) => !args.eventIds?.includes(event.id as string),
        );
      if (command.endsWith("_configure") && args.purge) state.events = [];
    },
  );
  state.queue.mockResolvedValue([]);
  state.version.mockResolvedValue("0.1.21");
  state.permission.mockResolvedValue({ status: "ok", data: "authorized" });
  state.requestSession.mockImplementation(async () => ({
    user: { id: state.userId },
    access_token: "fixture-token",
  }));
  state.rpc.mockImplementation(
    (name: string, args: { p_events?: Array<Record<string, unknown>> }) => ({
      setHeader: () => ({
        abortSignal: async () => {
          if (name !== "recording_diagnostics_ingest") return { error: null };
          if (state.offline) return { data: null, error: new Error("offline") };
          state.received.push(...(args.p_events ?? []));
          return {
            data: args.p_events?.map((event) => event.id) ?? [],
            error: null,
          };
        },
      }),
    }),
  );
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

it("retains offline diagnostic evidence until server acknowledgement and purges it on consent revocation", async () => {
  state.events = [
    {
      id: "event-1",
      kind: "policy_decision",
      reason: "mic_still_active",
      sessionId: "session-1",
    },
  ];
  state.offline = true;
  const view = render(<ApplicationHealthLifecycle />);
  await tick();
  expect(state.events).toHaveLength(1);
  expect(state.received).toEqual([]);
  state.offline = false;
  await act(async () => {
    await vi.advanceTimersByTimeAsync(15_000);
  });
  expect(state.received).toEqual([
    expect.objectContaining({ reason: "mic_still_active" }),
  ]);
  expect(state.events).toEqual([]);
  state.events = [
    { id: "event-2", kind: "stop_requested", reason: "manual_stop" },
  ];
  state.consent = false;
  view.rerender(<ApplicationHealthLifecycle />);
  await tick();
  expect(state.events).toEqual([]);
  expect(state.received).toHaveLength(1);
});

it("recovers diagnostic delivery after a native batch exceeds its deadline without sending the stale attempt", async () => {
  state.events = [
    { id: "event-1", kind: "policy_decision", reason: "mic_still_active" },
  ];
  const native = state.invoke.getMockImplementation()!;
  let finishBatch: ((value: unknown) => void) | undefined;
  state.invoke.mockImplementation((command: string, args: never) => {
    if (command.endsWith("_batch") && !finishBatch)
      return new Promise((resolve) => {
        finishBatch = resolve;
      });
    return native(command, args);
  });
  render(<ApplicationHealthLifecycle />);
  await tick();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10_001);
  });
  expect(state.received).toEqual([]);
  await act(async () => {
    finishBatch?.(await native("recording_diagnostics_batch", {}));
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(state.received).toEqual([expect.objectContaining({ id: "event-1" })]);
  expect(state.events).toEqual([]);
});
