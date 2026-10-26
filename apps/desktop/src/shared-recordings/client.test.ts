import { Blob } from "node:buffer";
import { webcrypto } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ fetch: vi.fn() }));
vi.mock("@tauri-apps/plugin-http", () => ({ fetch: mocks.fetch }));
vi.mock("~/env", () => ({
  env: { VITE_SHARED_RECORDINGS_PREVIEW_URL: "http://localhost:8789" },
}));

import { downloadSharedRecordingAudio } from "./client";

afterEach(() => {
  vi.unstubAllGlobals();
  mocks.fetch.mockReset();
});

it("plays only complete checksum-verified audio for the current account and rejects canceled downloads", async () => {
  vi.stubGlobal("Blob", Blob);
  vi.stubGlobal("crypto", webcrypto);
  const bytes = new Uint8Array([82, 73, 70, 70]);
  const checksum = Buffer.from(
    await webcrypto.subtle.digest("SHA-256", bytes),
  ).toString("hex");
  const input = {
    userId: "user",
    organizationId: "org",
    occurrenceId: "meeting",
    revision: "revision",
    audioSha256: checksum,
    accessToken: "token",
  };
  let owner = "user";
  let audio = () => new Response(bytes);
  mocks.fetch.mockImplementation(async (url: string) =>
    url.endsWith("/me")
      ? Response.json({ userId: owner, workspaces: [], connections: [] })
      : audio(),
  );
  const result = await downloadSharedRecordingAudio(
    input,
    new AbortController().signal,
  );
  expect(result.type).toBe("audio/wav");
  expect(new Uint8Array(await result.arrayBuffer())).toEqual(bytes);

  audio = () => new Response("tampered");
  await expect(
    downloadSharedRecordingAudio(input, new AbortController().signal),
  ).rejects.toThrow("checksum mismatch");
  audio = () =>
    new Response("oversized", {
      headers: { "content-length": String(512 * 1024 * 1024 + 1) },
    });
  await expect(
    downloadSharedRecordingAudio(input, new AbortController().signal),
  ).rejects.toThrow("playback size limit");
  audio = () => new Response(new Uint8Array());
  await expect(
    downloadSharedRecordingAudio(input, new AbortController().signal),
  ).rejects.toThrow("empty");
  audio = () => new Response("forbidden", { status: 403 });
  await expect(
    downloadSharedRecordingAudio(input, new AbortController().signal),
  ).rejects.toThrow("does not have access");
  owner = "other-user";
  await expect(
    downloadSharedRecordingAudio(input, new AbortController().signal),
  ).rejects.toThrow("Account does not match");

  const before = new AbortController();
  before.abort();
  await expect(
    downloadSharedRecordingAudio(input, before.signal),
  ).rejects.toThrow();
  const pending = new AbortController();
  let finish!: (response: Response) => void;
  mocks.fetch.mockImplementationOnce(
    () =>
      new Promise<Response>((resolve) => {
        finish = resolve;
      }),
  );
  const canceled = downloadSharedRecordingAudio(input, pending.signal);
  pending.abort();
  finish(Response.json({ userId: "user", workspaces: [], connections: [] }));
  await expect(canceled).rejects.toThrow();
});
