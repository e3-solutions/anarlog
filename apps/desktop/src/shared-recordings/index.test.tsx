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
vi.mock("./client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./client")>()),
  downloadSharedRecordingSource: mocks.downloadAudio,
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
      captures: [
        {
          captureId: `preview-${token}`,
          deleted: false,
          transcript: {
            provider: "asr",
            model: "model",
            result: { text: `Meeting preview for ${token}.` },
          },
        },
      ],
    },
  ],
  readSharedRecording: mocks.readRecording,
}));

import { SharedRecordingsAccount } from "./index";

beforeEach(() => {
  mocks.readRecording.mockImplementation(async (token: string) => ({
    occurrence: { provider: "slack_huddle" },
    captures: ["source-one", "source-two"].map((captureId, index) => ({
      captureId,
      deleted: false,
      frameCount: 16000,
      transcriptStatus: "ready",
      transcript: {
        provider: "asr",
        model: "model",
        result: {
          results: {
            channels: [
              {
                alternatives: [
                  { transcript: `Original source decision for ${token}.` },
                ],
              },
            ],
          },
        },
      },
      metadata: {
        uploaderId: index ? "second-contributor" : "first-contributor",
        segmentSequence: 0,
        sha256: captureId,
        offsetFrames: index * 16000,
        gaps: [],
      },
    })),
  }));
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
  expect(
    await screen.findByText("Meeting preview for account-one."),
  ).toBeTruthy();
  fireEvent.click(
    (await screen.findAllByRole("button", { name: "Listen" }))[0],
  );
  await waitFor(() => expect(signal).toBeDefined());
  mocks.userId = "account-two";
  view.rerender(view.content());
  expect(signal!.aborted).toBe(true);
  expect(view.container.querySelector("audio")).toBeNull();
  expect(screen.queryByText("Meeting preview for account-one.")).toBeNull();
  expect(
    await screen.findByText("Meeting preview for account-two."),
  ).toBeTruthy();
  fireEvent.click(await screen.findByRole("tab", { name: "Transcript" }));
  expect(
    await screen.findAllByText("Original source decision for account-two."),
  ).toHaveLength(2);
  view.client.clear();
});

it("shows original source transcripts and separate players, and retries failed source audio", async () => {
  mocks.downloadAudio.mockResolvedValue(
    new Blob(["RIFF"], { type: "audio/wav" }),
  );
  const view = renderAccount();
  fireEvent.click(await screen.findByRole("tab", { name: "Transcript" }));
  expect(
    await screen.findAllByText("Original source decision for account-one."),
  ).toHaveLength(2);
  fireEvent.click(screen.getByRole("tab", { name: "Recordings" }));
  fireEvent.click(
    (await screen.findAllByRole("button", { name: "Listen" }))[0],
  );
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
  fireEvent.click(screen.getByRole("button", { name: "Listen" }));
  await waitFor(() =>
    expect(view.container.querySelectorAll("audio")).toHaveLength(2),
  );
  view.unmount();
  view.client.clear();
});
