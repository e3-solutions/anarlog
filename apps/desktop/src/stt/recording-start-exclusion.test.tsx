import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  excluded: false,
  excludedKey: null as string | null,
  meeting: null as unknown,
  resolve: vi.fn(async (_owner: string, _input: { sessionId?: string }) => ({
    key: "call",
    room: null as string | null,
    kind: "session",
  })),
  replay: vi.fn(async () => []),
  prepare: vi.fn(async () => ({})),
  userId: "alice",
  authenticationReady: true,
  lifecycle: {
    ready: Promise.resolve(),
    deferCloudsync: vi.fn(),
    releaseCloudsyncLease: vi.fn(async () => {}),
    persistMarker: vi.fn(async () => {}),
    cleanupFailedStart: vi.fn(async () => {}),
  },
  start: vi.fn(async () => true),
  stop: vi.fn(),
  chat: vi.fn(),
  screen: vi.fn(),
}));
vi.mock("./automatic-meeting", () => ({
  selectAutomaticMeeting: () => state.meeting,
  sameAutomaticMeeting: (left: { key: string }, right: { key: string }) =>
    left.key === right.key,
}));
vi.mock("./meeting-recording-control", () => ({
  replayNativeRecordingExclusions: state.replay,
  prepareMeetingRecording: state.prepare,
}));
vi.mock("./capture-lifecycle", () => ({
  useCaptureLifecycle: () => ({
    authenticatedUserId: state.userId,
    authenticationReady: state.authenticationReady,
    conn: null,
    connectionReady: true,
    createCaptureLifecycle: () => state.lifecycle,
    session: null,
    setStopMeetingChatCapture: vi.fn(),
    stopMeetingChatTasks: vi.fn(async () => {}),
  }),
  CLOUDSYNC_CAPTURE_LEASE_ATTEMPTS: 1,
  getPostCaptureAction: vi.fn(),
  getPostCaptureRepairReasons: vi.fn(),
}));
vi.mock("./resume-listening", () => ({ useResumeListeningLifecycle: vi.fn() }));
vi.mock("./capture-lifecycle-storage", () => ({
  loadCaptureLifecycleMarker: vi.fn(async () => null),
  clearCaptureAudioSaved: vi.fn(),
  hasAudioAwaitingUser: () => false,
  hasPendingZeroRetentionAudio: () => false,
}));
vi.mock("./recording-exclusions", () => ({
  resolveRecordingOccurrence: state.resolve,
  bindRecordingSession: vi.fn(),
  isRecordingExcluded: (_owner: string, occurrence: { key: string }) =>
    state.excluded || occurrence.key === state.excludedKey,
  isRecordingSessionExcluded: () => state.excluded,
}));
vi.mock("./contexts", () => ({
  useListener: (selector: (store: unknown) => unknown) =>
    selector({
      start: state.start,
      stop: state.stop,
      canStartLiveSession: () => true,
      getSessionMode: () => "active",
      getLiveStartError: () => null,
    }),
}));
vi.mock("@anlg/plugin-detect", () => ({
  commands: {
    inspectMeetingAccessibility: vi.fn(async () => ({
      status: "ok",
      data: [],
    })),
  },
}));
vi.mock("@anlg/plugin-analytics", () => ({ commands: { event: vi.fn() } }));
vi.mock("@anlg/ui/components/ui/toast", () => ({
  toast: { warning: vi.fn() },
}));
vi.mock("~/analytics", () => ({ trackAnalyticsEvent: vi.fn() }));
vi.mock("~/contexts/shell", () => ({
  useShell: () => ({ leftsidebar: { setExpanded: vi.fn() } }),
}));
vi.mock("~/shared/config", () => ({ useConfigValue: () => undefined }));
vi.mock("~/settings/queries", () => ({ useSettingsReady: () => true }));
vi.mock("~/settings/general/language", () => ({
  getBaseLanguageDisplayName: vi.fn(),
}));
vi.mock("~/services/audio-retention", () => ({
  normalizeAudioRetention: () => "forever",
}));
vi.mock("~/store/zustand/tabs", () => ({
  useTabs: (selector: (store: unknown) => unknown) =>
    selector({ openNew: vi.fn() }),
}));
vi.mock("./capabilities", () => ({
  getLiveTranscriptionConfig: vi.fn(async () => ({
    languages: [],
    transcriptionMode: "batch",
  })),
  getTranscriptionLanguages: () => [],
  requiresRetainedBatchAudio: () => false,
}));
vi.mock("./queries", () => ({
  useSessionParticipantHumanIds: () => [],
  getSessionParticipantHumanIds: vi.fn(async () => []),
}));
vi.mock("./useKeywords", () => ({ getSessionKeywords: vi.fn(async () => []) }));
vi.mock("./meeting-chat-capture", () => ({
  startMeetingChatCapture: state.chat,
}));
vi.mock("./meeting-screen-capture", () => ({
  startMeetingScreenCapture: state.screen,
}));
vi.mock("./meeting-disclosure", () => ({
  MEETING_DISCLOSURE_MESSAGE: "Recording",
  startMeetingRecordingDisclosure: vi.fn(),
  sendMeetingRecordingDisclosure: vi.fn(),
}));
vi.mock("./primary-device", () => ({
  startPrimaryDeviceCoordination: vi.fn(),
}));
vi.mock("./start-failure", () => ({
  classifyStartFailure: vi.fn(),
  getMicrophonePermission: vi.fn(),
  showStartFailureToast: vi.fn(),
}));

import { useStartListening } from "./useStartListening";

beforeEach(() => {
  vi.clearAllMocks();
  state.excluded = false;
  state.excludedKey = null;
  state.meeting = null;
  state.resolve.mockResolvedValue({ key: "call", room: null, kind: "session" });
  state.replay.mockResolvedValue([]);
  state.prepare.mockResolvedValue({});
  state.userId = "alice";
  state.authenticationReady = true;
  state.lifecycle.ready = Promise.resolve();
  state.start.mockResolvedValue(true);
});

it("does not start capture when opt-out arrives while startup prerequisites are pending", async () => {
  let ready!: () => void;
  state.lifecycle.ready = new Promise<void>((resolve) => {
    ready = resolve;
  });
  const { result } = renderHook(() => useStartListening("session"));
  let pending!: Promise<void>;
  act(() => {
    pending = result.current();
  });
  await waitFor(() =>
    expect(state.lifecycle.deferCloudsync).toHaveBeenCalled(),
  );
  state.excluded = true;
  await act(async () => {
    ready();
    await pending;
  });
  expect(state.start).not.toHaveBeenCalled();
  expect(state.chat).not.toHaveBeenCalled();
  expect(state.screen).not.toHaveBeenCalled();
});

it("stops a capture completing after opt-out without starting chat or screen capture", async () => {
  let started!: (value: boolean) => void;
  state.start.mockImplementationOnce(
    () =>
      new Promise<boolean>((resolve) => {
        started = resolve;
      }),
  );
  const { result } = renderHook(() => useStartListening("session"));
  let pending!: Promise<void>;
  act(() => {
    pending = result.current();
  });
  await waitFor(() => expect(state.start).toHaveBeenCalled());
  state.excluded = true;
  await act(async () => {
    started(true);
    await pending;
  });
  expect(state.stop).toHaveBeenCalled();
  expect(state.chat).not.toHaveBeenCalled();
  expect(state.screen).not.toHaveBeenCalled();
});

it("waits for account restoration and abandons startup when the account changes", async () => {
  state.authenticationReady = false;
  const { result, rerender } = renderHook(() => useStartListening("session"));
  await act(async () => {
    await result.current();
  });
  expect(state.start).not.toHaveBeenCalled();
  state.authenticationReady = true;
  let ready!: () => void;
  state.lifecycle.ready = new Promise<void>((resolve) => {
    ready = resolve;
  });
  rerender();
  let pending!: Promise<void>;
  act(() => {
    pending = result.current();
  });
  await waitFor(() =>
    expect(state.lifecycle.deferCloudsync).toHaveBeenCalled(),
  );
  state.userId = "bob";
  rerender();
  await act(async () => {
    ready();
    await pending;
  });
  expect(state.start).not.toHaveBeenCalled();
});

it("replays a native privacy hold before a manual start can reach capture", async () => {
  state.replay.mockImplementationOnce(async () => {
    state.excluded = true;
    return [];
  });
  const { result } = renderHook(() => useStartListening("session"));
  await act(async () => {
    await result.current();
  });
  expect(state.start).not.toHaveBeenCalled();
  expect(state.prepare).not.toHaveBeenCalled();
});

it("prepares tray opt-out for the current call when recording from an older calendar note", async () => {
  const current = {
    key: "room:current",
    room: "https://meet.google.com/abc-defg-hij",
    kind: "room",
  };
  state.meeting = { key: "detected-current-call" };
  state.resolve.mockImplementation(async (_owner, input) =>
    input.sessionId
      ? {
          key: "calendar:older-note",
          room: "https://meet.google.com/xyz-abcd-uvw",
          kind: "calendar",
        }
      : current,
  );
  const { result } = renderHook(() => useStartListening("session"));
  await act(async () => {
    await result.current();
  });
  expect(state.prepare).toHaveBeenCalledWith("alice", "session", current);
  expect(state.start).toHaveBeenCalled();
});

it("abandons deferred startup when the user switches into an excluded call", async () => {
  state.meeting = { key: "allowed-call" };
  state.excludedKey = "excluded-call";
  state.resolve.mockImplementation(async () => ({
    key: (state.meeting as { key: string }).key,
    room: null,
    kind: "room",
  }));
  let ready!: () => void;
  state.lifecycle.ready = new Promise<void>((resolve) => {
    ready = resolve;
  });
  const { result } = renderHook(() => useStartListening("session"));
  let pending!: Promise<void>;
  act(() => {
    pending = result.current();
  });
  await waitFor(() =>
    expect(state.lifecycle.deferCloudsync).toHaveBeenCalled(),
  );
  state.meeting = { key: "excluded-call" };
  await act(async () => {
    ready();
    await pending;
  });
  expect(state.start).not.toHaveBeenCalled();
  expect(state.lifecycle.cleanupFailedStart).toHaveBeenCalled();
});
