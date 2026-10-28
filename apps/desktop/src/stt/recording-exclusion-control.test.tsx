import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  nativeExclude: vi.fn(),
  invoke: vi.fn(),
  create: vi.fn(),
  open: vi.fn(),
  stop: vi.fn(),
  execute: vi.fn(),
  live: { sessionId: "sensitive-session", status: "active" },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("~/auth", () => ({
  useOptionalAuth: () => ({ session: { user: { id: "alice" } } }),
}));
vi.mock("~/db", () => ({ liveQueryClient: { execute: mocks.execute } }));
vi.mock("~/session/queries", () => ({ createSession: mocks.create }));
vi.mock("~/shared-recordings/native", () => ({
  excludeSharedRecording: mocks.nativeExclude,
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: { getState: () => ({ live: mocks.live, stop: mocks.stop }) },
}));
vi.mock("~/store/zustand/tabs", () => ({
  useTabs: { getState: () => ({ openNew: mocks.open }) },
}));
vi.mock("@anlg/plugin-detect", () => ({
  commands: {
    inspectMeetingAccessibility: async () => {
      throw new Error("permission unavailable");
    },
  },
}));

import { RecordingExclusionControl } from "./recording-exclusion-control";
import {
  bindRecordingSession,
  excludeRecordingOccurrence,
  isRecordingSessionExcluded,
  resolveRecordingOccurrence,
} from "./recording-exclusions";

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
  mocks.nativeExclude.mockResolvedValue(null);
  mocks.live = { sessionId: "sensitive-session", status: "active" };
  mocks.stop.mockImplementation(() => {
    mocks.live.status = "inactive";
  });
  mocks.invoke.mockImplementation(async (command, args) =>
    command.endsWith("meeting_recording_exclusions") ? [] : args.context,
  );
  mocks.create.mockResolvedValue("fresh-session");
  mocks.execute.mockRejectedValue(new Error("database unavailable"));
});
afterEach(cleanup);
function show() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <RecordingExclusionControl sessionId="sensitive-session" />
    </QueryClientProvider>,
  );
}

it("keeps opt-out actionable when identity storage fails and stops the current recorder", async () => {
  show();
  await screen.findByText(/Meeting identity is unavailable/);
  fireEvent.click(
    screen.getByRole("button", { name: "Don't record this meeting" }),
  );
  await waitFor(() => expect(mocks.live.status).toBe("inactive"));
  expect(mocks.stop).toHaveBeenCalled();
  expect(isRecordingSessionExcluded("alice", "sensitive-session")).toBe(true);
  await screen.findByRole("button", { name: "Resume recording" });
});

it("resumes in a fresh session without releasing the excluded audio", async () => {
  mocks.live.status = "inactive";
  const occurrence = await resolveRecordingOccurrence("alice", {
    event: {
      calendar_id: "calendar",
      tracking_id: "occurrence",
      started_at: "2026-10-27T12:00:00Z",
    },
  });
  bindRecordingSession("alice", "sensitive-session", occurrence);
  excludeRecordingOccurrence("alice", occurrence, ["sensitive-session"]);
  show();
  fireEvent.click(
    await screen.findByRole("button", { name: "Resume recording" }),
  );
  await waitFor(() =>
    expect(mocks.open).toHaveBeenCalledWith({
      type: "sessions",
      id: "fresh-session",
      state: { view: null, autoStart: true, scheduledAutoStart: null },
    }),
  );
  expect(isRecordingSessionExcluded("alice", "sensitive-session")).toBe(true);
  expect(isRecordingSessionExcluded("alice", "fresh-session")).toBe(false);
});

it("does not resume or open a new recording after its account control unmounts", async () => {
  mocks.live.status = "inactive";
  const occurrence = await resolveRecordingOccurrence("alice", {
    event: {
      calendar_id: "calendar",
      tracking_id: "occurrence",
      started_at: "2026-10-27T12:00:00Z",
    },
  });
  bindRecordingSession("alice", "sensitive-session", occurrence);
  excludeRecordingOccurrence("alice", occurrence, ["sensitive-session"]);
  let finish!: (id: string) => void;
  let started!: () => void;
  const pending = new Promise<string>((resolve) => {
    finish = resolve;
  });
  const creating = new Promise<void>((resolve) => {
    started = resolve;
  });
  mocks.create.mockImplementation(() => {
    started();
    return pending;
  });
  const view = show();
  fireEvent.click(
    await screen.findByRole("button", { name: "Resume recording" }),
  );
  await creating;
  view.unmount();
  await act(async () => {
    finish("fresh-session");
    await pending;
  });
  expect(mocks.open).not.toHaveBeenCalled();
  expect(isRecordingSessionExcluded("alice", "sensitive-session")).toBe(true);
});
