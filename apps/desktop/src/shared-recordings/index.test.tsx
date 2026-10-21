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
  userId: "account-one",
  downloadAudio: vi.fn(),
  readRecording: vi.fn(),
}));

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
vi.mock("./native", () => ({
  downloadSharedRecordingAudio: mocks.downloadAudio,
}));
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
  readSharedRecording: mocks.readRecording,
  transcriptText: () => "",
}));

import { SharedRecordingsAccount } from "./index";

afterEach(() => {
  cleanup();
  mocks.userId = "account-one";
  mocks.downloadAudio.mockReset();
  mocks.readRecording.mockReset();
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

it("shows media delivery failures and reloads shared audio when retried", async () => {
  mocks.downloadAudio.mockResolvedValue("asset://localhost/shared.wav");
  mocks.readRecording.mockResolvedValue({
    occurrence: { provider: "slack_huddle" },
    captures: [],
    inputsChanged: false,
    current: {
      revision: "revision-one",
      number: 1,
      audioSha256: "checksum",
      transcriptStatus: "ready",
      transcript: null,
      manifest: {
        duration_frames: 16000,
        sample_rate: 16000,
        selections: [],
      },
    },
  });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const view = render(
    <QueryClientProvider client={client}>
      <SharedRecordingsAccount />
    </QueryClientProvider>,
  );
  fireEvent.click(await screen.findByText("conferenceRecords/account-one"));
  await waitFor(() =>
    expect(view.container.querySelector("audio")).toBeTruthy(),
  );
  const audio = view.container.querySelector("audio")!;
  Object.defineProperty(audio, "error", { value: { code: 4 } });
  fireEvent.error(audio);
  expect(screen.getByRole("alert").textContent).toContain("media error 4");
  expect(view.container.querySelector("audio")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  await waitFor(() =>
    expect(view.container.querySelector("audio")?.src).toBe(
      "asset://localhost/shared.wav",
    ),
  );
  client.clear();
});
