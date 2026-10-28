import { Blob } from "node:buffer";
import { webcrypto } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ fetch: vi.fn() }));
vi.mock("~/env", () => ({
  env: { VITE_API_URL: "http://localhost:8789" },
}));

import {
  downloadSharedRecordingSource,
  readSharedRecordingAccount,
  listSharedRecordings,
} from "./client";

afterEach(() => {
  vi.unstubAllGlobals();
  mocks.fetch.mockReset();
});

it("plays only complete checksum-verified audio for the current account and rejects canceled downloads", async () => {
  vi.stubGlobal("fetch", mocks.fetch);
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
    captureId: "source",
    sha256: checksum,
    accessToken: "token",
  };
  let owner = "user";
  let audio = () => new Response(bytes);
  mocks.fetch.mockImplementation(async (url: string) =>
    url.endsWith("/me")
      ? Response.json({ userId: owner, workspaces: [], connections: [] })
      : audio(),
  );
  const result = await downloadSharedRecordingSource(
    input,
    new AbortController().signal,
  );
  expect(result.type).toBe("audio/wav");
  expect(new Uint8Array(await result.arrayBuffer())).toEqual(bytes);

  audio = () => new Response("tampered");
  await expect(
    downloadSharedRecordingSource(input, new AbortController().signal),
  ).rejects.toThrow("checksum mismatch");
  audio = () =>
    new Response("oversized", {
      headers: { "content-length": String(512 * 1024 * 1024 + 1) },
    });
  await expect(
    downloadSharedRecordingSource(input, new AbortController().signal),
  ).rejects.toThrow("playback size limit");
  audio = () => new Response(new Uint8Array());
  await expect(
    downloadSharedRecordingSource(input, new AbortController().signal),
  ).rejects.toThrow("empty");
  audio = () => new Response("forbidden", { status: 403 });
  await expect(
    downloadSharedRecordingSource(input, new AbortController().signal),
  ).rejects.toThrow("does not have access");
  owner = "other-user";
  await expect(
    downloadSharedRecordingSource(input, new AbortController().signal),
  ).rejects.toThrow("Account does not match");

  mocks.fetch.mockResolvedValueOnce(new Response("Not found", { status: 404 }));
  await expect(readSharedRecordingAccount("token")).rejects.toThrow(
    "not configured on this server",
  );

  const before = new AbortController();
  before.abort();
  await expect(
    downloadSharedRecordingSource(input, before.signal),
  ).rejects.toThrow();
  const pending = new AbortController();
  let finish!: (response: Response) => void;
  mocks.fetch.mockImplementationOnce(
    () =>
      new Promise<Response>((resolve) => {
        finish = resolve;
      }),
  );
  const canceled = downloadSharedRecordingSource(input, pending.signal);
  pending.abort();
  finish(Response.json({ userId: "user", workspaces: [], connections: [] }));
  await expect(canceled).rejects.toThrow();
});

it("opts into unverified recordings and accepts legacy and uncertain audio timelines", async () => {
  vi.stubGlobal("fetch", mocks.fetch);
  const metadata = {
    uploaderId: "user",
    deviceId: "device",
    localCaptureId: "capture",
    sha256: "checksum",
    offsetFrames: 0,
    timingBasis: "device_clock_hint",
    gaps: [],
  };
  mocks.fetch.mockResolvedValueOnce(
    Response.json({
      items: [
        {
          occurrenceId: "local-session",
          occurrence: {
            provider: "unverified",
            providerOccurrenceId: "opaque",
          },
          captures: [
            { captureId: "legacy", deleted: false, metadata },
            {
              captureId: "uncertain",
              deleted: false,
              metadata: { ...metadata, audioTimelineVerified: false },
            },
          ],
        },
      ],
    }),
  );
  const recordings = await listSharedRecordings("token", "organization");
  expect(mocks.fetch.mock.calls[0][0]).toContain("?includeUnverified=true");
  expect(recordings[0].occurrence.provider).toBe("unverified");
  expect(
    recordings[0].captures[0].metadata.audioTimelineVerified,
  ).toBeUndefined();
  expect(recordings[0].captures[1].metadata.audioTimelineVerified).toBe(false);
});
