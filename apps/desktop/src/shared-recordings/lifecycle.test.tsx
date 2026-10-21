import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
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
}));

vi.mock("~/shared/integration", () => ({
  useOpenIntegrationUrl: () => ({
    openIntegration: mocks.openIntegration,
    openingAction: null,
  }),
}));
vi.mock("./native", () => ({
  sharedRecordingConfiguration: async () => mocks.saved,
  configureSharedRecordings: async (config: unknown) => {
    mocks.saved = config;
    return config;
  },
}));
vi.mock("./client", () => ({
  sharedRecordingsPreviewUrl: "http://localhost:8789",
}));

import { SharedRecordingPreferences } from "./lifecycle";

afterEach(() => {
  cleanup();
  mocks.saved = null;
  mocks.openIntegration.mockReset();
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
