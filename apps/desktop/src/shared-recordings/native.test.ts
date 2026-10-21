import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  convertFileSrc: (path: string) => `asset://localhost${path}`,
}));
vi.mock("./client", () => ({
  sharedRecordingsPreviewUrl: "http://localhost:8789",
}));

import { downloadSharedRecordingAudio } from "./native";

afterEach(() => {
  vi.unstubAllGlobals();
  mocks.invoke.mockReset();
});

it("loads verified cached WAV bytes for blob playback and rejects oversized or canceled loads", async () => {
  const input = {
    userId: "user",
    organizationId: "org",
    occurrenceId: "meeting",
    revision: "revision",
    audioSha256: "checksum",
    accessToken: "token",
  };
  mocks.invoke.mockResolvedValue("/verified.wav");
  const fetchAudio = vi
    .fn()
    .mockResolvedValue(new Response(new Uint8Array([82, 73, 70, 70])));
  vi.stubGlobal("fetch", fetchAudio);
  const result = await downloadSharedRecordingAudio(
    input,
    new AbortController().signal,
  );
  expect(result.type).toBe("audio/wav");
  expect(result.size).toBe(4);
  fetchAudio.mockResolvedValue(
    new Response("too large", {
      headers: { "content-length": String(512 * 1024 * 1024 + 1) },
    }),
  );
  await expect(
    downloadSharedRecordingAudio(input, new AbortController().signal),
  ).rejects.toThrow("playback size limit");
  const controller = new AbortController();
  controller.abort();
  await expect(
    downloadSharedRecordingAudio(input, controller.signal),
  ).rejects.toThrow();
  const pending = new AbortController();
  let finishDownload!: (path: string) => void;
  mocks.invoke.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        finishDownload = resolve;
      }),
  );
  const canceled = downloadSharedRecordingAudio(input, pending.signal);
  pending.abort();
  finishDownload("/verified.wav");
  await expect(canceled).rejects.toThrow();
});
