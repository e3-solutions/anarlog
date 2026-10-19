import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MeetingAccessibilityInspection } from "@anlg/plugin-detect";

import { useAutomaticMeetingRecording } from "./useAutomaticMeetingRecording";

import { createListenerStore } from "~/store/zustand/listener";

const mocks = vi.hoisted(() => ({
  inspect: vi.fn(),
  microphones: vi.fn(),
  create: vi.fn(),
  remove: vi.fn(),
  open: vi.fn(),
  config: vi.fn(),
}));

vi.mock("@anlg/plugin-detect", () => ({
  commands: {
    inspectMeetingAccessibility: mocks.inspect,
    listMicUsingApplications: mocks.microphones,
  },
}));
vi.mock("~/session/queries", () => ({
  createSession: mocks.create,
  softDeleteSession: mocks.remove,
}));
vi.mock("~/store/zustand/tabs", () => ({
  useTabs: { getState: () => ({ openNew: mocks.open }) },
}));
vi.mock("~/shared/config", () => ({ useConfigValue: mocks.config }));

const meet: MeetingAccessibilityInspection = {
  activeCall: true,
  app: { id: "com.google.Chrome", name: "Chrome" },
  pid: 123,
  platform: "googleMeet",
  surface: "web",
  accessibilityTrusted: true,
  windowTitle: "Meet - abc-defg-hij",
  remoteScreenShare: false,
  warnings: [],
};
const slack: MeetingAccessibilityInspection = {
  ...meet,
  platform: "slack",
  app: { id: "com.tinyspeck.slackmacgap", name: "Slack" },
  surface: "native",
  windowTitle: "Huddle",
};

function Harness({ store }: { store: ReturnType<typeof createListenerStore> }) {
  useAutomaticMeetingRecording(store);
  return null;
}

async function tick(ms = 2_000) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

function snapshot(data: MeetingAccessibilityInspection[]) {
  mocks.inspect.mockResolvedValue({ status: "ok", data });
}

function setup() {
  const store = createListenerStore();
  const stop = vi.fn(() => {
    store.setState((state) => ({
      live: { ...state.live, status: "inactive", loading: false },
    }));
  });
  store.setState({ stop });
  const view = render(<Harness store={store} />);
  const active = () => {
    store.setState((state) => ({
      live: {
        ...state.live,
        status: "active",
        sessionId: "session-1",
        loading: false,
      },
    }));
  };
  return { store, stop, active, view };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  mocks.config.mockImplementation((key: string) => {
    if (key === "ignored_platforms") return [];
    return (
      key === "auto_start_detected_meetings" || key === "auto_stop_meetings"
    );
  });
  mocks.microphones.mockResolvedValue({ status: "ok", data: [] });
  mocks.create.mockResolvedValue("session-1");
  mocks.remove.mockResolvedValue(undefined);
  snapshot([meet]);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("automatic local meeting recording", () => {
  it.each([meet, slack])(
    "starts joined $platform ad hoc or muted calls",
    async (inspection) => {
      snapshot([inspection]);
      const { store } = setup();
      await tick(0);
      expect(store.getState().live.triggerAppIds).toEqual([inspection.app.id]);
      expect(mocks.open).toHaveBeenCalledWith({
        type: "sessions",
        id: "session-1",
        state: { view: null, autoStart: true, scheduledAutoStart: null },
      });
    },
  );

  it.each([
    { inspections: [] },
    { inspections: [{ ...meet, activeCall: false }] },
    { inspections: [{ ...meet, platform: "unknown" as const }] },
    { inspections: [{ ...meet, accessibilityTrusted: false }] },
  ])(
    "does not infer attendance from an open browser or calendar",
    async ({ inspections }) => {
      snapshot(inspections);
      mocks.microphones.mockResolvedValue({ status: "ok", data: [meet.app] });
      setup();
      await tick(4_000);
      expect(mocks.open).not.toHaveBeenCalled();
    },
  );

  it("stops after confirmed absence and records the next call without a reminder cooldown", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([{ ...meet, activeCall: false }]);
    await tick(14_000);
    expect(stop).not.toHaveBeenCalled();
    await tick(4_000);
    expect(stop).toHaveBeenCalledOnce();
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
  });

  it("cancels stop grace when joined-call evidence returns while still muted", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([{ ...meet, activeCall: false }]);
    await tick(10_000);
    snapshot([meet]);
    await tick(20_000);
    expect(stop).not.toHaveBeenCalled();
    expect(mocks.open).toHaveBeenCalledOnce();
  });

  it("respects manual stop until the same call ends", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    stop();
    await tick(10_000);
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([{ ...meet, activeCall: false }]);
    await tick();
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
  });

  it("cleans up if attendance disappears during session creation, then retries", async () => {
    mocks.create.mockImplementationOnce(async () => {
      snapshot([{ ...meet, activeCall: false }]);
      return "unused";
    });
    setup();
    await tick(0);
    expect(mocks.remove).toHaveBeenCalledWith("unused");
    expect(mocks.open).not.toHaveBeenCalled();
    snapshot([meet]);
    await tick(4_000);
    expect(mocks.open).not.toHaveBeenCalled();
    await tick(2_000);
    expect(mocks.open).toHaveBeenCalledOnce();
  });

  it("cleans up a start that never reached the recorder and retries", async () => {
    setup();
    await tick(0);
    await tick(36_000);
    expect(mocks.remove).toHaveBeenCalledWith("session-1");
    expect(mocks.open).toHaveBeenCalledOnce();
    await tick(6_000);
    expect(mocks.open).toHaveBeenCalledTimes(2);
  });

  it("never stops a recording that has no detected-call ownership", async () => {
    snapshot([]);
    const { active, stop } = setup();
    active();
    await tick(40_000);
    expect(stop).not.toHaveBeenCalled();
  });

  it("keeps recording when Accessibility becomes unavailable but the call app still owns the mic", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([{ ...meet, activeCall: false, accessibilityTrusted: false }]);
    mocks.microphones.mockResolvedValue({ status: "ok", data: [meet.app] });
    await tick(40_000);
    expect(stop).not.toHaveBeenCalled();
  });

  it("preserves a muted listening call while far-end speech continues despite unreadable Accessibility", async () => {
    const { store, active, stop } = setup();
    await tick(0);
    active();
    snapshot([
      {
        ...meet,
        activeCall: false,
        warnings: ["no uniquely scoped meeting AXWindow"],
      },
    ]);
    for (let minute = 0; minute < 3; minute++) {
      store.setState((state) => ({
        live: { ...state.live, amplitude: { mic: 0, speaker: 0.2 } },
      }));
      await tick(30_000);
    }
    expect(stop).not.toHaveBeenCalled();
    expect(mocks.open).toHaveBeenCalledOnce();
  });

  it("holds unknown call evidence during a network interruption and stops on reliable leave after reconnect", async () => {
    const online = vi
      .spyOn(window.navigator, "onLine", "get")
      .mockReturnValue(true);
    try {
      const { active, stop } = setup();
      await tick(0);
      active();
      online.mockReturnValue(false);
      window.dispatchEvent(new Event("offline"));
      snapshot([]);
      mocks.microphones.mockResolvedValue({ status: "ok", data: [meet.app] });
      await tick(90_000);
      expect(stop).not.toHaveBeenCalled();

      online.mockReturnValue(true);
      window.dispatchEvent(new Event("online"));
      mocks.microphones.mockResolvedValue({ status: "ok", data: [] });
      // Reconnection alone must not discard the interruption hold.
      await tick(70_000);
      expect(stop).not.toHaveBeenCalled();

      snapshot([{ ...meet, activeCall: false }]);
      await tick(14_000);
      expect(stop).not.toHaveBeenCalled();
      await tick(4_000);
      expect(stop).toHaveBeenCalledOnce();
    } finally {
      online.mockRestore();
    }
  });

  it("does not restart a manually stopped call after unreadable local evidence", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    stop();
    snapshot([]);
    await tick(4_000);
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([]);
    await tick(64_000);
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([{ ...meet, activeCall: false }]);
    await tick();
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
  });

  it("respects ignored platforms even when their joined-call evidence is valid", async () => {
    mocks.config.mockImplementation((key: string) => {
      if (key === "ignored_platforms") return [meet.app.id];
      return (
        key === "auto_start_detected_meetings" || key === "auto_stop_meetings"
      );
    });
    setup();
    await tick(4_000);
    expect(mocks.open).not.toHaveBeenCalled();
  });

  it("supports the old Slack setting without enabling browser automatic capture", async () => {
    mocks.config.mockImplementation((key: string) => {
      if (key === "ignored_platforms") return [];
      return key === "auto_start_slack_huddles" || key === "auto_stop_meetings";
    });
    setup();
    await tick(4_000);
    expect(mocks.open).not.toHaveBeenCalled();
    snapshot([slack]);
    await tick();
    expect(mocks.open).toHaveBeenCalledOnce();
  });
});
