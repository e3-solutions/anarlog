import { z } from "zod";

import { env } from "~/env";

const transcriptSchema = z.object({
  provider: z.string(),
  model: z.string(),
  result: z.unknown(),
});

const occurrenceSchema = z.object({
  occurrenceId: z.string(),
  occurrence: z.object({
    provider: z.enum(["google_meet", "slack_huddle"]),
    providerOccurrenceId: z.string(),
  }),
  captures: z.array(
    z.object({
      captureId: z.string(),
      deleted: z.boolean(),
      frameCount: z.number().default(0),
      transcriptStatus: z.string().default("pending"),
      transcript: transcriptSchema.nullable().default(null),
      metadata: z.object({
        uploaderId: z.string(),
        deviceId: z.string(),
        localCaptureId: z.string(),
        segmentSequence: z.number().optional(),
        sha256: z.string(),
        offsetFrames: z.number(),
        timingBasis: z.string(),
        gaps: z
          .array(z.object({ start: z.number(), end: z.number() }))
          .default([]),
      }),
    }),
  ),
});

const accountSchema = z.object({
  userId: z.string(),
  workspaces: z.array(
    z.object({
      id: z.string(),
      name: z.string(),
      role: z.string(),
    }),
  ),
  policy: z
    .object({
      workspaceId: z.string(),
      autoRecordMeetings: z.boolean(),
      autoShareRecordings: z.boolean(),
    })
    .optional(),
  connections: z.array(
    z.object({
      integrationId: z.string(),
      connectionId: z.string(),
      status: z.string(),
    }),
  ),
});

export type SharedRecordingAccount = z.infer<typeof accountSchema>;
export type SharedRecordingOccurrence = z.infer<typeof occurrenceSchema>;
export type SharedRecordingSource =
  SharedRecordingOccurrence["captures"][number];

async function request(
  path: string,
  accessToken: string,
  signal?: AbortSignal,
  method: "GET" | "POST" = "GET",
) {
  signal?.throwIfAborted();
  const response = await fetch(
    `${env.VITE_API_URL.replace(/\/$/, "")}${path}`,
    {
      method,
      headers: { Authorization: `Bearer ${accessToken}` },
      redirect: "error",
      signal,
    },
  );
  signal?.throwIfAborted();
  if (!response.ok) {
    if (
      path === "/v1/shared-recordings/enroll" &&
      [403, 404].includes(response.status)
    )
      return response;
    if (response.status === 404 && path === "/v1/shared-recordings/me") {
      throw new Error("The meeting library is not configured on this server.");
    }
    if (response.status === 401) {
      throw new Error("Sign in again to access shared recordings.");
    }
    if (response.status === 403) {
      throw new Error(
        "Your account does not have access to this organization.",
      );
    }
    throw new Error(`Shared recording request failed (${response.status}).`);
  }
  return response;
}

export async function readSharedRecordingAccount(
  accessToken: string,
  signal?: AbortSignal,
) {
  return accountSchema.parse(
    await (
      await request("/v1/shared-recordings/me", accessToken, signal)
    ).json(),
  );
}

export async function enrollSharedRecordingAccount(accessToken: string) {
  const response = await request(
    "/v1/shared-recordings/enroll",
    accessToken,
    undefined,
    "POST",
  );
  if (!response.ok) return readSharedRecordingAccount(accessToken);
  return accountSchema.parse(await response.json());
}

export async function listSharedRecordings(
  accessToken: string,
  organizationId: string,
) {
  const response = await request(
    `/v1/orgs/${encodeURIComponent(organizationId)}/shared-recordings`,
    accessToken,
  );
  return z
    .object({ items: z.array(occurrenceSchema) })
    .parse(await response.json()).items;
}

export async function readSharedRecording(
  accessToken: string,
  organizationId: string,
  occurrenceId: string,
) {
  const response = await request(
    `/v1/orgs/${encodeURIComponent(organizationId)}/shared-recordings/${encodeURIComponent(occurrenceId)}`,
    accessToken,
  );
  return occurrenceSchema.parse(await response.json());
}

export function transcriptText(recording: {
  transcript?: { result: unknown } | null;
}): string {
  const result = z
    .object({
      text: z.string().optional(),
      results: z
        .object({
          channels: z.array(
            z.object({
              alternatives: z.array(
                z.object({ transcript: z.string().optional() }),
              ),
            }),
          ),
        })
        .optional(),
    })
    .safeParse(recording.transcript?.result);
  return result.success
    ? (result.data.text ??
        result.data.results?.channels[0]?.alternatives[0]?.transcript ??
        "")
    : "";
}

export async function downloadSharedRecordingSource(
  input: {
    userId: string;
    organizationId: string;
    occurrenceId: string;
    captureId: string;
    sha256: string;
    accessToken: string;
  },
  signal: AbortSignal,
) {
  signal.throwIfAborted();
  if (
    (await readSharedRecordingAccount(input.accessToken, signal)).userId !==
    input.userId
  ) {
    throw new Error("Account does not match shared recording.");
  }
  const response = await request(
    `/v1/orgs/${encodeURIComponent(input.organizationId)}/shared-recordings/${encodeURIComponent(input.occurrenceId)}/captures/${encodeURIComponent(input.captureId)}/audio`,
    input.accessToken,
    signal,
  );
  if (!response.body) throw new Error("Shared audio is empty.");
  const limit = 512 * 1024 * 1024;
  if (Number(response.headers.get("content-length")) > limit) {
    await response.body.cancel();
    throw new Error("Shared audio exceeds the playback size limit.");
  }
  const reader = response.body.getReader();
  const chunks: BlobPart[] = [];
  let length = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) {
        await reader.cancel();
        throw new Error("Shared audio exceeds the playback size limit.");
      }
      chunks.push(new Uint8Array(value));
    }
  } finally {
    reader.releaseLock();
  }
  signal.throwIfAborted();
  if (length === 0) throw new Error("Shared audio is empty.");
  const blob = new Blob(chunks, { type: "audio/wav" });
  const digest = await crypto.subtle.digest(
    "SHA-256",
    await blob.arrayBuffer(),
  );
  signal.throwIfAborted();
  const checksum = Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
  if (checksum !== input.sha256)
    throw new Error("Shared audio checksum mismatch.");
  return blob;
}
