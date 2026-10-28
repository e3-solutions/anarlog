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
  platform: vi.fn(),
  auth: vi.fn(),
  resolve: vi.fn(),
  excluded: vi.fn(),
  bind: vi.fn(),
  replay: vi.fn(),
  block: vi.fn(),
  diagnostic: vi.fn(),
}));

vi.mock("~/application-health/recording-diagnostics", () => ({
  recordRecordingDiagnostic: mocks.diagnostic,
}));

vi.mock("./meeting-recording-control", () => ({
  replayNativeRecordingExclusions: mocks.replay,
  stopMeetingRecordingForExcludedOccurrence: mocks.block,
  prepareMeetingRecording: vi.fn(async () => ({})),
}));

vi.mock("@tauri-apps/plugin-os", () => ({ platform: mocks.platform }));

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
vi.mock("~/auth", () => ({ useOptionalAuth: mocks.auth }));
vi.mock("./recording-exclusions", () => ({
  resolveRecordingOccurrence: mocks.resolve,
  isRecordingExcluded: mocks.excluded,
  bindRecordingSession: mocks.bind,
}));

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
  ...{ callIdentity: null },
};
const slack: MeetingAccessibilityInspection = {
  ...meet,
  platform: "slack",
  app: { id: "com.tinyspeck.slackmacgap", name: "Slack" },
  surface: "native",
  windowTitle: "Huddle",
};

const occurrence = {
  key: "calendar:abc-defg-hij",
  room: "https://meet.google.com/abc-defg-hij",
  kind: "calendar" as const,
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
  mocks.platform.mockReturnValue("macos");
  mocks.replay.mockResolvedValue([]);
  mocks.block.mockResolvedValue(undefined);
  mocks.config.mockImplementation((key: string) => {
    if (key === "ignored_platforms") return [];
    return (
      key === "auto_start_detected_meetings" || key === "auto_stop_meetings"
    );
  });
  mocks.microphones.mockResolvedValue({ status: "ok", data: [] });
  mocks.create.mockResolvedValue("session-1");
  mocks.remove.mockResolvedValue(undefined);
  // Default to no signed-in account so the exclusion gate stays dormant and
  // existing automatic-recording behavior is preserved; account tests override.
  mocks.auth.mockReturnValue(null);
  mocks.resolve.mockResolvedValue(occurrence);
  mocks.excluded.mockReturnValue(false);
  snapshot([meet]);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("automatic local meeting recording", () => {
  it("requires fresh confirmed Meet departure after unreadable polls and resets the grace on reappearance", async () => {
    const identity = "https://meet.google.com/abc-defg-hij:tab:42";
    const joined = {
      ...meet,
      callIdentity: identity,
      openCallIdentities: [identity],
    };
    snapshot([joined]);
    const { active, stop } = setup();
    await tick(0);
    active();
    const hidden = {
      ...meet,
      activeCall: false,
      platform: "unknown" as const,
      callIdentity: null,
      windowTitle: null,
      warnings: ["no uniquely scoped meeting AXWindow"],
    };
    snapshot([{ ...hidden, openCallIdentities: [] }]);
    await tick(8_000);
    snapshot([hidden]);
    await tick(12_000);
    expect(stop).not.toHaveBeenCalled();

    snapshot([joined]);
    await tick(2_000);
    snapshot([{ ...hidden, openCallIdentities: [] }]);
    await tick(14_000);
    expect(stop).not.toHaveBeenCalled();
    snapshot([hidden]);
    await tick(4_000);
    expect(stop).not.toHaveBeenCalled();
    snapshot([{ ...hidden, openCallIdentities: [] }]);
    await tick(2_000);
    expect(stop).toHaveBeenCalledOnce();
  });

  it("uses complete tab inventory to distinguish a hidden Meet from a closed tab while Chrome remains running", async () => {
    const identity = "https://meet.google.com/abc-defg-hij:tab:42";
    snapshot([{ ...meet, callIdentity: identity }]);
    const { active, stop } = setup();
    await tick(0);
    active();
    const hidden = {
      ...meet,
      activeCall: false,
      platform: "unknown" as const,
      callIdentity: null,
      windowTitle: null,
      warnings: ["no uniquely scoped meeting AXWindow"],
    };
    snapshot([{ ...hidden, ...{ openCallIdentities: [identity] } }]);
    await tick(90_000);
    expect(stop).not.toHaveBeenCalled();
    snapshot([hidden]);
    await tick(90_000);
    expect(stop).not.toHaveBeenCalled();
    snapshot([{ ...hidden, ...{ openCallIdentities: null } }]);
    await tick(90_000);
    expect(stop).not.toHaveBeenCalled();
    snapshot([{ ...hidden, ...{ openCallIdentities: [] } }]);
    await tick(14_000);
    expect(stop).not.toHaveBeenCalled();
    await tick(4_000);
    expect(stop).toHaveBeenCalledOnce();
    expect(mocks.open).toHaveBeenCalledOnce();
  });

  it("upgrades an owned window identity to the same Meet tab identity without duplicating capture", async () => {
    const url = "https://meet.google.com/abc-defg-hij";
    snapshot([{ ...meet, callIdentity: `${url}:window:abc` }]);
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([
      {
        ...meet,
        callIdentity: `${url}:tab:42`,
        ...{ openCallIdentities: [`${url}:tab:42`] },
      },
    ]);
    await tick(90_000);
    expect(stop).not.toHaveBeenCalled();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([
      {
        ...meet,
        activeCall: false,
        platform: "unknown",
        callIdentity: null,
        ...{ openCallIdentities: [] },
      },
    ]);
    await tick(18_000);
    expect(stop).toHaveBeenCalledOnce();
  });

  it("preserves a manual stop when the same Meet upgrades from window to tab identity", async () => {
    const url = "https://meet.google.com/abc-defg-hij";
    snapshot([{ ...meet, callIdentity: `${url}:window:abc` }]);
    const { active, stop } = setup();
    await tick(0);
    active();
    stop();
    const tab = {
      ...meet,
      callIdentity: `${url}:tab:42`,
      ...{ openCallIdentities: [`${url}:tab:42`] },
    };
    snapshot([tab]);
    await tick(90_000);
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([{ ...tab, activeCall: false, ...{ openCallIdentities: [] } }]);
    await tick();
    snapshot([tab]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
  });

  it("preserves hidden silent context A, ignores ended B, then stops only A and records B", async () => {
    const a = { ...meet, callIdentity: "window-a:meet-a" };
    const b = { ...meet, callIdentity: "window-b:meet-b" };
    const presence = {
      ...meet,
      activeCall: false,
      platform: "unknown" as const,
    };
    snapshot([a, presence]);
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([presence]);
    await tick(9 * 60_000);
    snapshot([presence, { ...b, activeCall: false }]);
    await tick(90_000);
    expect(stop).not.toHaveBeenCalled();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([presence, { ...a, activeCall: false }, b]);
    await tick(14_000);
    expect(stop).not.toHaveBeenCalled();
    await tick(4_000);
    expect(stop).toHaveBeenCalledOnce();
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
  });

  it("keeps independent manual stops for contexts A and B until each positively departs", async () => {
    const a = { ...meet, callIdentity: "window-a:meet-a" };
    const b = { ...meet, callIdentity: "window-b:meet-b" };
    snapshot([a]);
    const { active, stop } = setup();
    await tick(0);
    active();
    stop();
    snapshot([b]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
    active();
    stop();
    snapshot([a]);
    await tick(70_000);
    expect(mocks.open).toHaveBeenCalledTimes(2);
    snapshot([{ ...a, activeCall: false }]);
    await tick();
    snapshot([b]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(2);
    snapshot([a]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(3);
    active();
    stop();
    snapshot([{ ...b, activeCall: false }]);
    await tick();
    snapshot([b]);
    await tick();
    expect(mocks.open).toHaveBeenCalledTimes(4);
  });

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

  it("reports a stalled AX probe without claiming that recording stopped", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    mocks.inspect.mockImplementation(() => new Promise(() => {}));
    await tick(20_000);
    expect(mocks.diagnostic).toHaveBeenCalledWith(
      "session-1",
      "policy_decision",
      "probe_timeout",
      expect.objectContaining({
        inspectionStatus: "pending",
        micStatus: "ok",
        owned: true,
        probeElapsedMs: expect.any(Number),
      }),
    );
    expect(stop).not.toHaveBeenCalled();
  });

  it("preserves a muted listening call while far-end speech continues despite unreadable Accessibility", async () => {
    snapshot([slack]);
    const { store, active, stop } = setup();
    await tick(0);
    active();
    snapshot([
      {
        ...slack,
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

  it("keeps a quiet muted Meet recording across hidden-tab evidence loss until departure is confirmed", async () => {
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([
      {
        ...meet,
        platform: "unknown",
        activeCall: false,
        warnings: ["no uniquely scoped meeting AXWindow"],
      },
    ]);
    await tick(90_000);
    await tick(9 * 60_000);
    expect(stop).not.toHaveBeenCalled();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([{ ...meet, activeCall: false }]);
    await tick(14_000);
    expect(stop).not.toHaveBeenCalled();
    await tick(4_000);
    expect(stop).toHaveBeenCalledOnce();
  });

  it.each(["macos", "linux", "windows"])(
    "treats a missing browser process as confirmed departure only on macOS: %s",
    async (os) => {
      mocks.platform.mockReturnValue(os);
      const { active, stop } = setup();
      await tick(0);
      active();
      snapshot([{ ...meet, pid: meet.pid + 1, activeCall: false }]);
      await tick(14_000);
      expect(stop).not.toHaveBeenCalled();
      await tick(4_000);
      if (os === "macos") expect(stop).toHaveBeenCalledOnce();
      else {
        expect(stop).not.toHaveBeenCalled();
        await tick(90_000);
        expect(stop).toHaveBeenCalledOnce();
      }
    },
  );

  it("retains the Slack fallback stop when call evidence and microphone ownership remain unavailable", async () => {
    snapshot([slack]);
    const { active, stop } = setup();
    await tick(0);
    active();
    snapshot([]);
    await tick(58_000);
    expect(stop).not.toHaveBeenCalled();
    expect(mocks.diagnostic).toHaveBeenCalledWith(
      "session-1",
      "policy_decision",
      "ax_unknown",
      expect.objectContaining({
        observation: "unknown",
        inspectionStatus: "ok",
        micStatus: "ok",
        micActive: false,
        owned: true,
        autoStop: true,
        speakerRecent: true,
      }),
    );
    await tick(6_000);
    expect(stop).toHaveBeenCalledOnce();
    expect(mocks.diagnostic).toHaveBeenCalledWith(
      "session-1",
      "stop_requested",
      "grace_elapsed",
      expect.objectContaining({ observation: "unknown", owned: true }),
    );
  });

  it("holds unknown call evidence during a network interruption and stops on reliable leave after reconnect", async () => {
    const online = vi
      .spyOn(window.navigator, "onLine", "get")
      .mockReturnValue(true);
    try {
      snapshot([slack]);
      const { active, stop } = setup();
      await tick(0);
      active();
      online.mockReturnValue(false);
      window.dispatchEvent(new Event("offline"));
      snapshot([]);
      mocks.microphones.mockResolvedValue({ status: "ok", data: [slack.app] });
      await tick(90_000);
      expect(stop).not.toHaveBeenCalled();

      online.mockReturnValue(true);
      window.dispatchEvent(new Event("online"));
      mocks.microphones.mockResolvedValue({ status: "ok", data: [] });
      // Reconnection alone must not discard the interruption hold.
      await tick(70_000);
      expect(stop).not.toHaveBeenCalled();

      snapshot([{ ...slack, activeCall: false }]);
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
    const unreadable = {
      ...meet,
      activeCall: false,
      accessibilityTrusted: false,
    };
    snapshot([unreadable]);
    await tick(4_000);
    snapshot([meet]);
    await tick();
    expect(mocks.open).toHaveBeenCalledOnce();
    snapshot([unreadable]);
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

  it.each(["macos", "linux", "windows"])(
    "clears manual-stop suppression on successful process absence only on macOS: %s",
    async (os) => {
      mocks.platform.mockReturnValue(os);
      const { active, stop } = setup();
      await tick(0);
      active();
      stop();
      mocks.inspect.mockResolvedValue({
        status: "error",
        error: "inspection failed",
      });
      await tick(64_000);
      snapshot([meet]);
      await tick();
      expect(mocks.open).toHaveBeenCalledOnce();
      snapshot([]);
      await tick();
      snapshot([meet]);
      await tick();
      expect(mocks.open).toHaveBeenCalledTimes(os === "macos" ? 2 : 1);
    },
  );

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

  it("does not enable either platform from an obsolete Slack setting", async () => {
    mocks.config.mockImplementation((key: string) => {
      if (key === "ignored_platforms") return [];
      return key === "auto_start_slack_huddles" || key === "auto_stop_meetings";
    });
    setup();
    await tick(4_000);
    expect(mocks.open).not.toHaveBeenCalled();
    snapshot([slack]);
    await tick();
    expect(mocks.open).not.toHaveBeenCalled();
  });

  it("replays native exclusions before creating a detector recording", async () => {
    mocks.auth.mockReturnValue({ session: { user: { id: "account-1" } } });
    mocks.replay.mockImplementationOnce(async () => {
      mocks.excluded.mockReturnValue(true);
      return [];
    });
    setup();
    await tick(0);
    expect(mocks.create).not.toHaveBeenCalled();
    expect(mocks.open).not.toHaveBeenCalled();
  });

  it("stops an owned capture immediately when the selected call switches to an excluded occurrence", async () => {
    mocks.auth.mockReturnValue({ session: { user: { id: "account-1" } } });
    const { active, stop } = setup();
    await tick(0);
    active();
    const blocked = { ...occurrence, key: "calendar:excluded-call" };
    mocks.resolve.mockResolvedValue(blocked);
    mocks.excluded.mockReturnValue(true);
    snapshot([
      {
        ...meet,
        callIdentity: "https://meet.google.com/xyz-abcd-uvw:tab:99",
      },
    ]);
    await tick();
    expect(mocks.block).toHaveBeenCalledWith("account-1", "session-1", blocked);
    expect(stop).toHaveBeenCalled();
    expect(mocks.create).toHaveBeenCalledOnce();
  });

  it("never arms an excluded occurrence across polling, rejoin, and remount", async () => {
    mocks.auth.mockReturnValue({ session: { user: { id: "account-1" } } });
    mocks.excluded.mockReturnValue(true);
    snapshot([meet]);
    const { store, view } = setup();
    await tick(0);
    expect(mocks.excluded).toHaveBeenCalledWith("account-1", occurrence);
    expect(mocks.create).not.toHaveBeenCalled();
    expect(mocks.open).not.toHaveBeenCalled();
    expect(store.getState().live.triggerAppIds).toBeNull();

    // Keep polling: the opt-out must hold rather than decay into a start.
    await tick(90_000);
    expect(mocks.create).not.toHaveBeenCalled();
    expect(mocks.open).not.toHaveBeenCalled();

    // Leave and rejoin the same call: a fresh occurrence stays excluded.
    snapshot([]);
    await tick(4_000);
    snapshot([meet]);
    await tick(4_000);
    expect(mocks.create).not.toHaveBeenCalled();
    expect(mocks.open).not.toHaveBeenCalled();

    // Remounting the hook (e.g. window reopen) must not re-arm it either.
    view.unmount();
    render(<Harness store={store} />);
    await tick(4_000);
    expect(mocks.create).not.toHaveBeenCalled();
    expect(mocks.open).not.toHaveBeenCalled();
    expect(store.getState().live.triggerAppIds).toBeNull();
  });

  it("drops a deferred session when the opt-out lands during createSession", async () => {
    mocks.auth.mockReturnValue({ session: { user: { id: "account-1" } } });
    mocks.excluded.mockReturnValue(false);
    mocks.create.mockImplementationOnce(async () => {
      // The user opts out while the session is still being created.
      mocks.excluded.mockReturnValue(true);
      return "session-1";
    });
    snapshot([meet]);
    const { store } = setup();
    await tick(0);
    expect(mocks.create).toHaveBeenCalledOnce();
    expect(mocks.bind).toHaveBeenCalledWith(
      "account-1",
      "session-1",
      occurrence,
    );
    expect(mocks.remove).toHaveBeenCalledWith("session-1");
    expect(mocks.open).not.toHaveBeenCalled();
    expect(store.getState().live.triggerAppIds).toBeNull();
  });
});
