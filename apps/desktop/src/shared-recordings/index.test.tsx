import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
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
          frameCount: 16000,
          transcriptStatus: "ready",
          metadata: {
            uploaderId: token,
            deviceId: "preview-device",
            localCaptureId: "preview-recording",
            sha256: `preview-${token}`,
            offsetFrames: 0,
            timingBasis: "device_clock_hint",
            gaps: [],
          },
          transcript: {
            provider: "asr",
            model: "model",
            result: { text: `Meeting preview for ${token}.` },
          },
        },
      ],
    },
    {
      occurrenceId: "someone-elses-meeting",
      occurrence: { provider: "slack_huddle" },
      captures: [{ deleted: false, metadata: { uploaderId: "another-user" } }],
    },
  ],
  readSharedRecording: mocks.readRecording,
}));

import { SharedRecordingsAccount } from "./index";

beforeEach(() => {
  mocks.readRecording.mockImplementation(async (token: string) => ({
    occurrence: { provider: "slack_huddle" },
    captures: ["source-one", "source-two", "foreign-source"].map(
      (captureId, index) => ({
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
                    {
                      transcript: `${index === 2 ? "Private teammate transcript" : `Original source decision for ${token}.`}`,
                    },
                  ],
                },
              ],
            },
          },
        },
        metadata: {
          uploaderId: index === 2 ? "another-user" : token,
          deviceId: "same-device",
          localCaptureId: captureId,
          timingBasis: "device_clock_hint",
          segmentSequence: 0,
          sha256: captureId,
          offsetFrames: index * 16000,
          gaps: [],
        },
      }),
    ),
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
  await screen.findByRole("button", { name: /Google Meet/ });
  fireEvent.click(
    (await screen.findAllByRole("button", { name: "Listen" }))[0],
  );
  await waitFor(() => expect(signal).toBeDefined());
  mocks.userId = "account-two";
  view.rerender(view.content());
  expect(signal!.aborted).toBe(true);
  expect(view.container.querySelector("audio")).toBeNull();
  expect(screen.queryByText("Meeting preview for account-one.")).toBeNull();
  await screen.findByRole("button", { name: /Google Meet/ });
  expect(await screen.findAllByRole("button", { name: "Listen" })).toHaveLength(
    2,
  );
  view.client.clear();
});

it("shows only your audio and transcripts, and retries failed source audio", async () => {
  mocks.downloadAudio.mockResolvedValue(
    new Blob(["RIFF"], { type: "audio/wav" }),
  );
  const view = renderAccount();
  expect(await screen.findAllByRole("button", { name: "Listen" })).toHaveLength(
    2,
  );
  fireEvent.click(screen.getByRole("tab", { name: "Transcript" }));
  expect(
    screen.getAllByText("Original source decision for account-one."),
  ).toHaveLength(2);
  expect(screen.queryByText("Private teammate transcript")).toBeNull();
  fireEvent.click(screen.getByRole("tab", { name: "Recordings" }));
  expect(
    screen
      .getAllByRole("navigation", { name: "Meetings" })[0]
      .querySelectorAll("button"),
  ).toHaveLength(1);
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

it("groups your upload parts without exposing another contributor’s audio", async () => {
  const recording = await mocks.readRecording("account-one");
  const first = recording.captures[0];
  recording.captures.push({
    ...first,
    captureId: "earlier-part",
    metadata: {
      ...first.metadata,
      sha256: "earlier-part",
      offsetFrames: -16000,
    },
  });
  mocks.readRecording.mockResolvedValue(recording);
  mocks.downloadAudio.mockResolvedValue(
    new Blob(["RIFF"], { type: "audio/wav" }),
  );
  const view = renderAccount();
  await screen.findAllByRole("button", { name: "Listen" });
  const panel = screen.getByRole("tabpanel");
  const recordings = within(panel).getAllByRole("listitem");
  expect(recordings).toHaveLength(2);
  const firstRecording = within(recordings[0]);
  expect(
    firstRecording.getAllByRole("button", { name: "Listen" }),
  ).toHaveLength(2);
  expect(
    within(recordings[1]).getAllByRole("button", { name: "Listen" }),
  ).toHaveLength(1);
  fireEvent.click(firstRecording.getAllByRole("button", { name: "Listen" })[0]);
  await waitFor(() =>
    expect(recordings[0].querySelectorAll("audio")).toHaveLength(1),
  );
  expect(mocks.downloadAudio.mock.calls[0][0].captureId).toBe("earlier-part");
  fireEvent.click(firstRecording.getByRole("button", { name: "Listen" }));
  fireEvent.click(
    within(recordings[1]).getByRole("button", { name: "Listen" }),
  );
  await waitFor(() =>
    expect(view.container.querySelectorAll("audio")).toHaveLength(3),
  );
  view.unmount();
  view.client.clear();
});
