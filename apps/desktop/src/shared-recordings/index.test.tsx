import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ userId: "account-one" }));

vi.mock("~/auth", () => ({
  useAuth: () => ({
    session: { user: { id: mocks.userId, is_anonymous: false } },
    getSessionForRequest: async () => ({
      user: { id: mocks.userId },
      access_token: mocks.userId,
    }),
    signIn: async () => {},
  }),
}));
vi.mock("./lifecycle", () => ({
  SharedRecordingPreferences: () => null,
  SharedRecordingQueue: () => null,
}));
vi.mock("./native", () => ({ downloadSharedRecordingAudio: vi.fn() }));
vi.mock("./client", () => ({
  readSharedRecordingAccount: async (token: string) => ({
    userId: token,
    workspaces: [
      { id: "same-organization", name: "Organization", role: "member" },
    ],
    connections: [],
  }),
  listSharedRecordings: async (token: string) => [
    {
      occurrenceId: token,
      occurrence: {
        provider: "google_meet",
        providerOccurrenceId: `conferenceRecords/${token}`,
        durationFrames: 16000,
      },
      captures: [],
      current: null,
      inputsChanged: false,
    },
  ],
  readSharedRecording: vi.fn(),
  transcriptText: () => "",
}));

import { SharedRecordingsAccount } from "./index";

afterEach(() => {
  cleanup();
  mocks.userId = "account-one";
});

it("removes the previous account's recording list when the signed-in account changes", async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const content = () => (
    <QueryClientProvider client={client}>
      <SharedRecordingsAccount />
    </QueryClientProvider>
  );
  const view = render(content());
  await waitFor(() =>
    expect(screen.getByText("conferenceRecords/account-one")).toBeTruthy(),
  );
  mocks.userId = "account-two";
  view.rerender(content());
  expect(screen.queryByText("conferenceRecords/account-one")).toBeNull();
  await waitFor(() =>
    expect(screen.getByText("conferenceRecords/account-two")).toBeTruthy(),
  );
  expect(screen.queryByText("conferenceRecords/account-one")).toBeNull();
  client.clear();
});
