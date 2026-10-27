import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { act } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  userId: "account-one",
  values: {} as Record<string, boolean | string>,
  config: null as null | {
    enabled: boolean;
    apiBaseUrl: string;
    organizationId: string;
    userId: string;
    connections: {
      integrationId: string;
      connectionId: string;
      status: string;
    }[];
  },
  live: { status: "inactive", loading: false },
  getSettings: vi.fn(),
  configure: vi.fn(),
  writeSettings: vi.fn(),
  openIntegration: vi.fn(),
}));
const account = vi.hoisted(() => ({
  userId: "account-one",
  workspaces: [{ id: "organization", name: "E3", role: "member" }],
  connections: [
    {
      integrationId: "google-meet",
      connectionId: "meet-consent",
      status: "connected",
    },
    {
      integrationId: "google-calendar",
      connectionId: "meet-consent",
      status: "connected",
    },
    {
      integrationId: "slack",
      connectionId: "slack-consent",
      status: "connected",
    },
  ],
  policy: {
    workspaceId: "organization",
    autoRecordMeetings: true as const,
    autoShareRecordings: true as const,
  },
}));
vi.mock("~/auth", () => ({
  useAuth: () => ({
    getSessionForRequest: async () => ({
      user: { id: mocks.userId },
      access_token: mocks.userId,
    }),
  }),
}));
vi.mock("~/env", () => ({
  env: { VITE_API_URL: "https://recordings.example.com" },
}));
vi.mock("./client", () => ({
  enrollSharedRecordingAccount: async () => structuredClone(account),
  readSharedRecordingAccount: async () => structuredClone(account),
}));
vi.mock("./native", () => ({
  configureSharedRecordings: mocks.configure,
  sharedRecordingConfiguration: async () => mocks.config,
}));
vi.mock("@anlg/ui/components/ui/button", () => ({
  Button: ({
    variant: _variant,
    ...props
  }: { variant?: string } & React.ButtonHTMLAttributes<HTMLButtonElement>) => (
    <button {...props} />
  ),
}));
vi.mock("@anlg/ui/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => (
    <div role="dialog">{children}</div>
  ),
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <h2>{children}</h2>
  ),
  DialogDescription: ({ children }: { children: React.ReactNode }) => (
    <p>{children}</p>
  ),
}));
vi.mock("~/shared/integration", () => ({
  useOpenIntegrationUrl: () => ({
    openIntegration: mocks.openIntegration,
    openingAction: null,
  }),
}));
vi.mock("~/settings/queries", () => ({
  getStoredSettingValues: mocks.getSettings,
  setSettingValues: mocks.writeSettings,
  useStoredSettingValuesQuery: () => ({
    data: {
      values: mocks.values,
      hasValues: new Set(Object.keys(mocks.values)),
    },
    isLoading: false,
    error: null,
  }),
}));
vi.mock("~/stt/contexts", () => ({
  useListener: (selector: (state: { live: typeof mocks.live }) => unknown) =>
    selector({ live: mocks.live }),
}));
vi.mock("~/store/zustand/listener/instance", () => ({
  listenerStore: { getState: () => ({ live: mocks.live }) },
}));

import { SharedRecordingEnrollment } from "./enrollment";

const clients: QueryClient[] = [];
function openEnrollment() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  clients.push(client);
  return render(
    <QueryClientProvider client={client}>
      <SharedRecordingEnrollment userId="account-one" />
    </QueryClientProvider>,
  );
}
beforeEach(() => {
  mocks.userId = "account-one";
  account.connections = [
    {
      integrationId: "google-meet",
      connectionId: "meet-consent",
      status: "connected",
    },
    {
      integrationId: "google-calendar",
      connectionId: "meet-consent",
      status: "connected",
    },
    {
      integrationId: "slack",
      connectionId: "slack-consent",
      status: "connected",
    },
  ];
  mocks.values = {};
  mocks.config = null;
  mocks.live = { status: "inactive", loading: false };
  mocks.getSettings.mockImplementation(async () => ({
    values: { ...mocks.values },
    hasValues: new Set(Object.keys(mocks.values)),
  }));
  mocks.configure.mockImplementation(async (config) => {
    mocks.config = config;
    return config;
  });
  mocks.writeSettings.mockImplementation(async (updates) => {
    Object.assign(mocks.values, updates);
  });
});
afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.clearAllMocks();
});

it("reviews reused connections before enabling defaults and preserves later opt-outs", async () => {
  const first = openEnrollment();
  await screen.findByRole("button", { name: "Google connected" });
  expect(mocks.config).toBeNull();
  expect(mocks.values.auto_start_detected_meetings).toBeUndefined();
  fireEvent.click(screen.getByRole("button", { name: "Finish setup" }));
  await waitFor(() =>
    expect(mocks.values.auto_start_detected_meetings).toBe(true),
  );
  expect(mocks.config?.enabled).toBe(true);
  expect(mocks.values.auto_stop_meetings).toBe(true);
  expect(mocks.values.autostart).toBe(true);
  const marker = mocks.values.shared_recording_defaults_applied;
  first.unmount();
  mocks.values.auto_start_detected_meetings = false;
  mocks.values.autostart = false;
  mocks.config!.enabled = false;
  account.connections = account.connections.filter(
    (connection) => connection.integrationId !== "google-calendar",
  );
  await act(async () => {
    openEnrollment();
  });
  expect(mocks.values.auto_start_detected_meetings).toBe(false);
  expect(mocks.config?.enabled).toBe(false);
  expect(mocks.values.autostart).toBe(false);
  expect(mocks.values.shared_recording_defaults_applied).toBe(marker);
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("retains explicit recording and sharing opt-outs on first enrollment", async () => {
  mocks.values = {
    auto_start_detected_meetings: false,
    auto_stop_meetings: false,
    autostart: false,
  };
  mocks.config = {
    enabled: false,
    userId: account.userId,
    organizationId: "organization",
    apiBaseUrl: "https://recordings.example.com",
    connections: account.connections,
  };
  openEnrollment();
  fireEvent.click(await screen.findByRole("button", { name: "Finish setup" }));
  await waitFor(() =>
    expect(typeof mocks.values.shared_recording_defaults_applied).toBe(
      "string",
    ),
  );
  expect(mocks.values.auto_start_detected_meetings).toBe(false);
  expect(mocks.values.auto_stop_meetings).toBe(false);
  expect(mocks.values.autostart).toBe(false);
  expect(mocks.config.enabled).toBe(false);
});

it("abandons pending defaults when the active account changes before local configuration", async () => {
  let resume!: (value: {
    values: Record<string, boolean | string>;
    hasValues: Set<string>;
  }) => void;
  mocks.getSettings.mockImplementation(
    () =>
      new Promise((resolve) => {
        resume = resolve;
      }),
  );
  openEnrollment();
  fireEvent.click(await screen.findByRole("button", { name: "Finish setup" }));
  await waitFor(() => expect(resume).toBeDefined());
  await act(async () => {
    mocks.userId = "account-two";
    resume({ values: {}, hasValues: new Set() });
  });
  expect(mocks.config).toBeNull();
  expect(mocks.values).toEqual({});
});

it("finishes Google setup without a Slack connection", async () => {
  account.connections = [];
  openEnrollment();
  await screen.findByRole("dialog");
  fireEvent.click(screen.getByRole("button", { name: "Connect Google" }));
  expect(mocks.openIntegration).toHaveBeenCalledWith({
    nangoIntegrationId: "google-meet",
    connectionId: undefined,
    action: "connect",
  });
  account.connections = [
    {
      integrationId: "google-meet",
      connectionId: "meet-consent",
      status: "connected",
    },
  ];
  await act(async () => {
    clients[clients.length - 1].setQueryData(
      ["shared-recordings", account.userId, "account"],
      { ...account },
    );
  });
  fireEvent.click(
    await screen.findByRole("button", { name: "Connect Google" }),
  );
  expect(mocks.openIntegration).toHaveBeenLastCalledWith({
    nangoIntegrationId: "google-meet",
    connectionId: "meet-consent",
    action: "reconnect",
  });
  expect(mocks.config).toBeNull();
  account.connections.push({
    integrationId: "google-calendar",
    connectionId: "meet-consent",
    status: "connected",
  });
  await act(async () => {
    clients[clients.length - 1].setQueryData(
      ["shared-recordings", account.userId, "account"],
      structuredClone(account),
    );
  });
  await screen.findByRole("button", { name: "Google connected" });
  expect(mocks.config).toBeNull();
  expect(mocks.values.auto_start_detected_meetings).toBeUndefined();
  expect(screen.queryByRole("button", { name: "Connect Slack" })).toBeNull();
  expect(
    screen
      .getByRole("button", { name: "Finish setup" })
      .hasAttribute("disabled"),
  ).toBe(false);
  expect(mocks.config).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Finish setup" }));
  await waitFor(() =>
    expect(mocks.values.auto_start_detected_meetings).toBe(true),
  );
  expect(mocks.config?.enabled).toBe(true);
  expect(screen.queryByRole("dialog")).toBeNull();
});
