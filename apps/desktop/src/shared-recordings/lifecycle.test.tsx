import {
  focusManager,
  QueryClient,
  QueryClientProvider,
} from "@tanstack/react-query";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  saved: null as unknown,
  openIntegration: vi.fn(),
  observe: vi.fn(),
  flush: vi.fn(),
  queue: vi.fn(
    async () => [] as { id: string; sessionId: string; state: string }[],
  ),
  exclude: vi.fn(async (_sessionId: string, _userId: string) => {}),
  inspection: vi.fn(),
  uploaded: false,
  quarantined: false,
  authUnavailable: false,
  live: { status: "active", sessionId: "session-one" },
}));

vi.mock("~/stt/meeting-recording-control", () => ({
  replayNativeRecordingExclusions: async () => [],
}));
vi.mock("~/auth", () => ({
  useAuth: () => ({
    session: { user: { id: "account-one" } },
    getSessionForRequest: async () => {
      if (mocks.authUnavailable) throw new Error("offline");
      return {
        user: { id: "account-one" },
        access_token: "token",
      };
    },
  }),
}));
vi.mock("./enrollment", () => ({
  SharedRecordingEnrollment: () => null,
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: {
    getState: () => ({ live: mocks.live }),
  },
}));
vi.mock("@anlg/plugin-detect", () => ({
  commands: { inspectMeetingAccessibility: mocks.inspection },
}));
vi.mock("~/shared/integration", () => ({
  useOpenIntegrationUrl: () => ({
    openIntegration: mocks.openIntegration,
    openingAction: null,
  }),
}));
vi.mock("./native", () => ({
  observeSharedRecording: mocks.observe,
  flushSharedRecordings: mocks.flush,
  sharedRecordingConfiguration: async () => mocks.saved,
  configureSharedRecordings: async (config: unknown) => {
    mocks.saved = config;
    return config;
  },
  sharedRecordingQueue: mocks.queue,
  excludeSharedRecording: mocks.exclude,
}));
vi.mock("~/env", () => ({
  env: { VITE_API_URL: "http://localhost:8789" },
}));

import { SharedRecordingLifecycle } from "./lifecycle";
import { SharedRecordingPreferences, SharedRecordingQueue } from "./settings";

afterEach(() => {
  cleanup();
  localStorage.clear();
  mocks.queue.mockReset().mockResolvedValue([]);
  mocks.exclude.mockReset().mockResolvedValue(undefined);
  mocks.saved = null;
  mocks.openIntegration.mockReset();
  mocks.live = { status: "active", sessionId: "session-one" };
  mocks.quarantined = false;
  mocks.authUnavailable = false;
  mocks.observe.mockReset();
  mocks.flush.mockReset();
  mocks.inspection.mockReset();
  focusManager.setFocused(undefined);
  vi.useRealTimers();
});

it("keeps unverified active captures local without quarantining a stopped capture from a stale inspection", async () => {
  mocks.saved = {
    enabled: true,
    userId: "account-one",
    organizationId: "organization-one",
    apiBaseUrl: "http://localhost:8789",
    connections: [],
  };
  mocks.observe.mockImplementation(async (input) => {
    if (input.platform === "unverified") mocks.quarantined = true;
  });
  mocks.authUnavailable = true;
  mocks.inspection.mockResolvedValue({
    status: "error",
    error: "AX unavailable",
  });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const view = render(
    <QueryClientProvider client={client}>
      <SharedRecordingQueue userId="account-one" />
      <SharedRecordingLifecycle />
    </QueryClientProvider>,
  );
  await waitFor(() => expect(mocks.quarantined).toBe(true));
  await waitFor(() =>
    expect(screen.getByRole("status").textContent).toContain(
      "start a new recording",
    ),
  );
  view.unmount();
  client.clear();
  mocks.quarantined = false;
  let finishInspection!: (inspection: {
    status: "error";
    error: string;
  }) => void;
  let inspectionStarted!: () => void;
  const started = new Promise<void>((resolve) => {
    inspectionStarted = resolve;
  });
  mocks.inspection.mockImplementationOnce(() => {
    inspectionStarted();
    return new Promise((resolve) => {
      finishInspection = resolve;
    });
  });
  render(
    <QueryClientProvider client={client}>
      <SharedRecordingQueue userId="account-one" />
      <SharedRecordingLifecycle />
    </QueryClientProvider>,
  );
  await started;
  mocks.live = { status: "inactive", sessionId: "session-one" };
  await act(async () => {
    finishInspection({ status: "error", error: "AX unavailable" });
  });
  expect(screen.queryByRole("status")).toBeNull();
  expect(mocks.quarantined).toBe(false);
  cleanup();
  client.clear();
  mocks.live = { status: "active", sessionId: "session-two" };
  mocks.inspection.mockResolvedValue({
    status: "ok",
    data: [
      {
        activeCall: true,
        accessibilityTrusted: true,
        warnings: [],
        platform: "googleMeet",
        app: { id: "com.google.Chrome", name: "Chrome" },
        pid: 123,
        callIdentity: "https://meet.google.com/abc-defg-hij:window:aaa",
      },
    ],
  });
  const authFailure = render(
    <QueryClientProvider client={client}>
      <SharedRecordingQueue userId="account-one" />
      <SharedRecordingLifecycle />
    </QueryClientProvider>,
  );
  await waitFor(() => expect(mocks.quarantined).toBe(true));
  mocks.quarantined = false;
  authFailure.unmount();
  expect(mocks.quarantined).toBe(true);
  client.clear();
});

it("newly connected Slack accounts can enable sharing for their selected organization", async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const content = (connected: boolean) => (
    <QueryClientProvider client={client}>
      <SharedRecordingPreferences
        organizationId="organization-one"
        account={{
          userId: "account-one",
          workspaces: [],
          connections: connected
            ? [
                {
                  integrationId: "slack",
                  connectionId: "slack-one",
                  status: "connected",
                },
              ]
            : [],
        }}
      />
    </QueryClientProvider>
  );
  const view = render(content(false));
  await waitFor(() =>
    expect(
      screen
        .getByRole("button", { name: "Enable sharing" })
        .hasAttribute("disabled"),
    ).toBe(true),
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect Slack" }));
  expect(mocks.openIntegration).toHaveBeenCalledWith({
    nangoIntegrationId: "slack",
    connectionId: undefined,
    action: "connect",
  });

  view.rerender(content(true));
  const enable = screen.getByRole("button", { name: "Enable sharing" });
  await waitFor(() => expect(enable.hasAttribute("disabled")).toBe(false));
  fireEvent.click(enable);
  await waitFor(() =>
    expect(mocks.saved).toEqual({
      enabled: true,
      apiBaseUrl: "http://localhost:8789",
      organizationId: "organization-one",
      userId: "account-one",
      connections: [
        {
          integrationId: "slack",
          connectionId: "slack-one",
          status: "connected",
        },
      ],
    }),
  );
  client.clear();
});

it("recovers identity and uploads while two account views of the same Meet are in the background", async () => {
  mocks.saved = {
    enabled: true,
    userId: "account-one",
    organizationId: "organization-one",
    apiBaseUrl: "http://localhost:8789",
    connections: [],
  };
  let bound = false;
  mocks.uploaded = false;
  const rows = ["aaa", "bbb"].map((window) => ({
    activeCall: true,
    accessibilityTrusted: true,
    warnings: [],
    platform: "googleMeet",
    windowTitle: "Meet - abc-defg-hij",
    app: { id: "com.google.Chrome", name: "Chrome" },
    pid: 123,
    callIdentity: `https://meet.google.com/abc-defg-hij:window:${window}`,
  }));
  mocks.inspection.mockResolvedValue({ status: "ok", data: rows });
  mocks.observe
    .mockRejectedValueOnce(new Error("offline"))
    .mockImplementation(async () => {
      bound = true;
    });
  mocks.flush.mockImplementation(async () => {
    if (bound) mocks.uploaded = true;
  });
  vi.useFakeTimers();
  focusManager.setFocused(false);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <SharedRecordingQueue userId="account-one" />
      <SharedRecordingLifecycle />
    </QueryClientProvider>,
  );
  await vi.waitFor(() =>
    expect(screen.getByRole("status").textContent).toContain(
      "sharing will retry",
    ),
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(12_000);
  });
  expect(mocks.uploaded).toBe(true);
  client.clear();
});

it.each([
  ["matching inventory", "", true],
  ["missing inventory", "inventory_missing", true],
  ["other meeting inventory", "inventory_conflict", true],
  ["competing call", "call_changed", true],
  ["explicit departure", "meeting_departed", true],
  ["no native admission proof", "meeting_unverified", false],
] as const)(
  "keeps background capture bound only after verified admission: %s",
  async (scenario, reason, proof) => {
    const room = "https://meet.google.com/abc-defg-hij";
    const inventory = [`${room}:tab:42`];
    const row = {
      activeCall: true,
      accessibilityTrusted: true,
      windowTitle: "Meet - abc-defg-hij",
      warnings: [] as string[],
      platform: "googleMeet",
      app: { id: "com.google.Chrome", name: "Chrome" },
      pid: 123,
      callIdentity: `${room}:tab:42`,
      openCallIdentities: inventory,
    };
    mocks.saved = {
      enabled: true,
      userId: "account-one",
      organizationId: "organization-one",
      apiBaseUrl: "http://localhost:8789",
      connections: [
        {
          integrationId: "google-meet",
          connectionId: "google-one",
          status: "connected",
        },
      ],
    };
    mocks.observe.mockImplementation(async (input) => {
      if (input.platform === "unverified") {
        mocks.quarantined = true;
        return null;
      }
      if (mocks.quarantined) throw new Error("Original remains local");
      return proof ? "verified-occurrence" : null;
    });
    mocks.live = { status: "inactive", sessionId: "session-one" };
    mocks.inspection.mockResolvedValue({
      status: "error",
      error: "AX warming",
    });
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    render(
      <QueryClientProvider client={client}>
        <SharedRecordingQueue userId="account-one" />
        <SharedRecordingLifecycle />
      </QueryClientProvider>,
    );
    const queryKey = ["shared-recording-observation", "account-one"];
    await waitFor(() => expect(mocks.inspection).toHaveBeenCalled());
    await act(async () => {
      await client.refetchQueries({ queryKey });
    });
    expect(mocks.quarantined).toBe(false);
    mocks.live = { status: "active", sessionId: "session-one" };
    mocks.inspection.mockResolvedValue({ status: "ok", data: [row] });
    await act(async () => {
      await client.refetchQueries({ queryKey });
    });
    expect(mocks.quarantined).toBe(false);
    const background = {
      ...row,
      activeCall: false,
      platform: "unknown",
      windowTitle: null,
      warnings: [
        "browser inspection found no uniquely scoped meeting AXWindow and active AXWebArea",
      ],
      callIdentity: null,
      openCallIdentities:
        scenario === "missing inventory"
          ? undefined
          : scenario === "other meeting inventory"
            ? ["https://meet.google.com/xyz-abcd-uvw:tab:43"]
            : inventory,
    };
    mocks.inspection.mockResolvedValue({
      status: "ok",
      data:
        scenario === "competing call"
          ? [background, { ...row, platform: "slack", callIdentity: null }]
          : scenario === "explicit departure"
            ? [
                {
                  ...background,
                  platform: "googleMeet",
                  callIdentity: row.callIdentity,
                },
              ]
            : [background],
    });
    await act(async () => {
      await client.refetchQueries({ queryKey });
    });
    if (!reason) {
      expect(screen.queryByRole("status")).toBeNull();
      expect(mocks.quarantined).toBe(false);
    } else {
      await waitFor(() =>
        expect(screen.getByRole("status").textContent).toContain(
          "start a new recording",
        ),
      );
      expect(mocks.quarantined).toBe(true);
    }
    client.clear();
  },
);

it("replays persisted opt-out before a restarted upload pump can send pending audio", async () => {
  localStorage.setItem(
    "e3.recording-exclusions.v1",
    JSON.stringify({
      "account-one": {
        excluded: {},
        sessions: { "excluded-session": true },
        bindings: {},
      },
    }),
  );
  mocks.saved = {
    enabled: true,
    userId: "account-one",
    organizationId: "organization-one",
    apiBaseUrl: "http://localhost:8789",
    connections: [],
  };
  mocks.live = { status: "inactive", sessionId: "excluded-session" };
  mocks.inspection.mockResolvedValue({ status: "ok", data: [] });
  mocks.queue.mockResolvedValue([
    { id: "pending-job", sessionId: "excluded-session", state: "pending" },
  ]);
  let blockCompleted = false;
  let finishBlock!: () => void;
  mocks.exclude.mockImplementationOnce(async () => {
    await new Promise<void>((resolve) => {
      finishBlock = resolve;
    });
    blockCompleted = true;
  });
  mocks.uploaded = false;
  mocks.flush.mockImplementation(async () => {
    if (!blockCompleted) mocks.uploaded = true;
  });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <SharedRecordingLifecycle />
    </QueryClientProvider>,
  );
  await waitFor(() =>
    expect(mocks.exclude).toHaveBeenCalledWith(
      "excluded-session",
      "account-one",
    ),
  );
  expect(mocks.flush).not.toHaveBeenCalled();
  await act(async () => {
    finishBlock();
  });
  await waitFor(() =>
    expect(mocks.flush).toHaveBeenCalledWith("token", "account-one"),
  );
  expect(mocks.uploaded).toBe(false);
  client.clear();
});
