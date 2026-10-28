import { beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  replay: vi.fn(),
  resume: vi.fn(),
  stop: vi.fn(),
  live: { sessionId: "current" },
}));
vi.mock("~/auth", () => ({ useOptionalAuth: () => null }));
vi.mock("~/stt/meeting-recording-control", () => ({
  replayNativeRecordingExclusions: mocks.replay,
}));
vi.mock("~/stt/recording-exclusion-actions", () => ({
  resumeMeetingRecording: mocks.resume,
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: { getState: () => ({ live: mocks.live, stop: mocks.stop }) },
}));

import { handleMeetingRecordingControl } from "./tray-recording-controls";

import type { NativeMeetingContext } from "~/stt/meeting-recording-control";

const context: NativeMeetingContext = {
  userId: "alice",
  sessionId: "current",
  revision: "current-revision",
  state: "excluded",
  occurrence: { key: "room:one", room: "room-one", kind: "room" },
};
beforeEach(() => {
  vi.clearAllMocks();
  mocks.replay.mockResolvedValue([context]);
  mocks.resume.mockResolvedValue(undefined);
});

it("ignores stale or wrong-account menu events without stopping or resuming a recorder", async () => {
  for (const action of ["excluded", "resume"] as const) {
    for (const eventContext of [
      { ...context, userId: "bob" },
      { ...context, revision: "stale-revision" },
      { ...context, sessionId: "previous-session" },
    ]) {
      await handleMeetingRecordingControl(
        { action, context: eventContext, error: null },
        "alice",
        () => true,
      );
    }
  }
  expect(mocks.stop).not.toHaveBeenCalled();
  expect(mocks.resume).not.toHaveBeenCalled();
});

it("rechecks the account after replay before applying a menu event", async () => {
  let current = true;
  mocks.replay.mockImplementation(async () => {
    current = false;
    return [context];
  });
  await handleMeetingRecordingControl(
    { action: "excluded", context, error: null },
    "alice",
    () => current,
  );
  await handleMeetingRecordingControl(
    { action: "resume", context, error: null },
    "alice",
    () => current,
  );
  expect(mocks.stop).not.toHaveBeenCalled();
  expect(mocks.resume).not.toHaveBeenCalled();
});

it("applies only a matching durable hold and passes its revision to intentional resume", async () => {
  await handleMeetingRecordingControl(
    { action: "excluded", context, error: null },
    "alice",
    () => true,
  );
  expect(mocks.stop).toHaveBeenCalled();
  await handleMeetingRecordingControl(
    { action: "resume", context, error: null },
    "alice",
    () => true,
  );
  expect(mocks.resume).toHaveBeenCalledWith(
    expect.objectContaining({
      userId: "alice",
      expected: context,
      occurrence: context.occurrence,
    }),
  );
});
