import { beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  create: vi.fn(),
  open: vi.fn(),
  live: { status: "inactive", sessionId: null as string | null },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("~/session/queries", () => ({ createSession: mocks.create }));
vi.mock("~/shared-recordings/native", () => ({
  excludeSharedRecording: vi.fn(),
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: { getState: () => ({ live: mocks.live }) },
}));
vi.mock("~/store/zustand/tabs", () => ({
  useTabs: { getState: () => ({ openNew: mocks.open }) },
}));

import {
  replayNativeRecordingExclusions,
  prepareMeetingRecording,
  stopMeetingRecordingForExcludedOccurrence,
  type NativeMeetingContext,
} from "./meeting-recording-control";
import { resumeMeetingRecording } from "./recording-exclusion-actions";
import {
  bindRecordingSession,
  isRecordingExcluded,
  isRecordingSessionExcluded,
  type RecordingOccurrence,
} from "./recording-exclusions";

const occurrence: RecordingOccurrence = {
  key: "calendar:one",
  room: "https://meet.google.com/abc-defg-hij",
  kind: "calendar",
};
const hold: NativeMeetingContext = {
  userId: "alice",
  sessionId: "old",
  occurrence,
  revision: "native-hold",
  state: "excluded",
};

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
  mocks.live = { status: "inactive", sessionId: null };
  mocks.create.mockResolvedValue("fresh");
  mocks.invoke.mockImplementation(async (command, args) =>
    command.endsWith("meeting_recording_exclusions") ? [hold] : args.context,
  );
});

it("replays the native privacy hold before creating a fresh resumed session and keeps old audio blocked", async () => {
  mocks.create.mockImplementation(async () => {
    expect(isRecordingExcluded("alice", occurrence)).toBe(true);
    expect(isRecordingSessionExcluded("alice", "old")).toBe(true);
    return "fresh";
  });
  await resumeMeetingRecording({
    userId: "alice",
    occurrence,
    isCurrent: () => true,
    expected: hold,
  });
  expect(isRecordingExcluded("alice", occurrence)).toBe(false);
  expect(isRecordingSessionExcluded("alice", "old")).toBe(true);
  expect(isRecordingSessionExcluded("alice", "fresh")).toBe(false);
  expect(mocks.open).toHaveBeenCalledWith(
    expect.objectContaining({ id: "fresh" }),
  );
});

it("restores the original broader hold and blocks fresh-session recovery if native resume is not acknowledged", async () => {
  const broaderHold: NativeMeetingContext = {
    ...hold,
    occurrence: { key: "unidentified", room: null, kind: "ambiguous" },
  };
  mocks.invoke.mockImplementation(async (command) => {
    if (command.endsWith("meeting_recording_exclusions")) return [broaderHold];
    throw new Error("native unavailable");
  });
  await expect(
    resumeMeetingRecording({
      userId: "alice",
      occurrence,
      isCurrent: () => true,
      expected: broaderHold,
    }),
  ).rejects.toThrow("native unavailable");
  expect(isRecordingExcluded("alice", occurrence)).toBe(true);
  expect(isRecordingSessionExcluded("alice", "old")).toBe(true);
  expect(isRecordingSessionExcluded("alice", "fresh")).toBe(true);
  expect(
    isRecordingExcluded("alice", {
      key: "room:another",
      room: "another",
      kind: "room",
    }),
  ).toBe(true);
  expect(mocks.open).not.toHaveBeenCalled();
});

it("does not reapply an old polled hold while native resume acknowledgement is pending", async () => {
  let acknowledge!: () => void;
  let pending!: () => void;
  let acknowledged = false;
  const release = new Promise<void>((resolve) => {
    acknowledge = resolve;
  });
  const started = new Promise<void>((resolve) => {
    pending = resolve;
  });
  mocks.invoke.mockImplementation(async (command, args) => {
    if (command.endsWith("meeting_recording_exclusions"))
      return acknowledged ? [] : [hold];
    pending();
    await release;
    acknowledged = true;
    return args.context;
  });
  const resume = resumeMeetingRecording({
    userId: "alice",
    occurrence,
    isCurrent: () => true,
    expected: hold,
  });
  await started;
  const poll = replayNativeRecordingExclusions("alice");
  acknowledge();
  await Promise.all([resume, poll]);
  expect(isRecordingSessionExcluded("alice", "old")).toBe(true);
  expect(isRecordingSessionExcluded("alice", "fresh")).toBe(false);
  expect(isRecordingExcluded("alice", occurrence)).toBe(false);
});

it("rebinds a resumed note to the call actually present before capture and invalidates the earlier menu token", async () => {
  await resumeMeetingRecording({
    userId: "alice",
    occurrence,
    isCurrent: () => true,
    expected: hold,
  });
  const oldContext = mocks.invoke.mock.calls.find(([command]) =>
    command.endsWith("meeting_recording_control_set"),
  )![1].context;
  const currentCall: RecordingOccurrence = {
    key: "room:current",
    room: "https://meet.google.com/new-room-abc",
    kind: "room",
  };
  const context = await prepareMeetingRecording("alice", "fresh", currentCall);
  expect(context.occurrence).toEqual(currentCall);
  expect(context.revision).not.toBe(oldContext.revision);
  expect(isRecordingSessionExcluded("alice", "old")).toBe(true);
});

it("blocks a running session that crosses into an excluded call without excluding its previous allowed occurrence", async () => {
  const allowed: RecordingOccurrence = {
    ...occurrence,
    key: "calendar:allowed",
  };
  bindRecordingSession("alice", "crossed-session", allowed);
  const before = await prepareMeetingRecording(
    "alice",
    "crossed-session",
    allowed,
  );
  await stopMeetingRecordingForExcludedOccurrence(
    "alice",
    "crossed-session",
    occurrence,
  );
  expect(isRecordingSessionExcluded("alice", "crossed-session")).toBe(true);
  expect(isRecordingExcluded("alice", occurrence)).toBe(true);
  expect(isRecordingExcluded("alice", allowed)).toBe(false);
  const after =
    mocks.invoke.mock.calls[mocks.invoke.mock.calls.length - 1][1].context;
  expect(after.occurrence).toEqual(occurrence);
  expect(after.state).toBe("excluded");
  expect(after.revision).not.toBe(before.revision);
});
