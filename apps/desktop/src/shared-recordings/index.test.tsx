import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

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
vi.mock("./settings", () => ({
  SharedRecordingPreferences: () => null,
  SharedRecordingQueue: () => null,
}));
vi.mock("./client", () => ({
  downloadSharedRecordingAudio: mocks.downloadAudio,
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

beforeEach(() => {
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
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL() {
        return "blob:shared-audio";
      }
      static revokeObjectURL() {}
    },
  );
});

afterEach(() => {
  cleanup();
  mocks.userId = "account-one";
  mocks.downloadAudio.mockReset();
  mocks.readRecording.mockReset();
  vi.unstubAllGlobals();
});

function renderAccount() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const content = () => (
    <QueryClientProvider client={client}>
      <SharedRecordingsAccount />
    </QueryClientProvider>
  );
  return { ...render(content()), client, content };
}

it("cancels the previous account's audio and removes its recordings when accounts change", async () => {
  let signal: AbortSignal | undefined;
  mocks.downloadAudio.mockImplementation((_input, requestSignal) => {
    signal = requestSignal;
    return new Promise(() => {});
  });
  const view = renderAccount();
  fireEvent.click(await screen.findByText("conferenceRecords/account-one"));
  await waitFor(() => expect(signal).toBeDefined());
  mocks.userId = "account-two";
  view.rerender(view.content());
  expect(signal!.aborted).toBe(true);
  expect(view.container.querySelector("audio")).toBeNull();
  expect(screen.queryByText("conferenceRecords/account-one")).toBeNull();
  expect(await screen.findByText("conferenceRecords/account-two")).toBeTruthy();
  view.client.clear();
});

it("shows media delivery failures and reloads shared audio when retried", async () => {
  mocks.downloadAudio.mockResolvedValue(
    new Blob(["RIFF"], { type: "audio/wav" }),
  );
  const view = renderAccount();
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
    expect(view.container.querySelector("audio")).toBeTruthy(),
  );
  view.unmount();
  view.client.clear();
});
