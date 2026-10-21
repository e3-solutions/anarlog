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
  inspection: vi.fn(),
  uploaded: false,
}));

vi.mock("~/auth", () => ({
  useAuth: () => ({
    session: { user: { id: "account-one" } },
    getSessionForRequest: async () => ({
      user: { id: "account-one" },
      access_token: "token",
    }),
  }),
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: {
    getState: () => ({ live: { status: "active", sessionId: "session-one" } }),
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
}));
vi.mock("./client", () => ({
  sharedRecordingsPreviewUrl: "http://localhost:8789",
}));

import {
  SharedRecordingLifecycle,
  SharedRecordingPreferences,
} from "./lifecycle";

afterEach(() => {
  cleanup();
  mocks.saved = null;
  mocks.openIntegration.mockReset();
  focusManager.setFocused(undefined);
  vi.useRealTimers();
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
      <SharedRecordingLifecycle />
    </QueryClientProvider>,
  );
  await vi.waitFor(() =>
    expect(
      client.getQueryState(["shared-recording-observation", "account-one"])
        ?.status,
    ).toBe("error"),
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(12_000);
  });
  expect(mocks.uploaded).toBe(true);
  client.clear();
});
