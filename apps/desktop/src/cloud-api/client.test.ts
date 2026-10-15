import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  fetch: vi.fn(),
  getSession: vi.fn(),
  refreshSession: vi.fn(),
  getCloudSnapshot: vi.fn(),
  listCloudSnapshotIds: vi.fn(),
  execute: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-http", () => ({ fetch: mocks.fetch }));
vi.mock("@anlg/plugin-local-api", () => ({
  commands: {
    getCloudSnapshot: mocks.getCloudSnapshot,
    listCloudSnapshotIds: mocks.listCloudSnapshotIds,
  },
}));
vi.mock("~/auth/client", () => ({
  supabase: {
    auth: {
      getSession: mocks.getSession,
      refreshSession: mocks.refreshSession,
    },
  },
}));
vi.mock("~/env", () => ({
  env: { VITE_API_URL: "https://api.anarlog.test" },
}));
vi.mock("~/db", () => ({ liveQueryClient: { execute: mocks.execute } }));

const storedValues = new Map<string, string>();
vi.stubGlobal("localStorage", {
  clear: () => storedValues.clear(),
  getItem: (key: string) => storedValues.get(key) ?? null,
  removeItem: (key: string) => storedValues.delete(key),
  setItem: (key: string, value: string) => storedValues.set(key, value),
});

import {
  backfillCloudApiSnapshots,
  CloudApiClientError,
  deleteCloudApiSnapshotBestEffort,
  getCloudApiSettings,
  initializeCloudApiBackfill,
  scheduleCloudApiSnapshotSync,
  syncCloudApiSnapshot,
  syncCloudApiSnapshotBestEffort,
} from "./client";

function authSession(userId: string) {
  return {
    data: {
      session: {
        access_token: `token-${userId}`,
        user: { id: userId, is_anonymous: false },
      },
    },
    error: null,
  };
}

describe("cloud API client", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    mocks.fetch.mockResolvedValue(
      new Response(JSON.stringify({ enabled: true, updated_at: null }), {
        status: 200,
      }),
    );
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "ok",
      data: { id: "meeting-1", transcripts: [] },
    });
    mocks.listCloudSnapshotIds.mockResolvedValue({
      status: "ok",
      data: [],
    });
    mocks.execute.mockResolvedValue([]);
  });

  it("publishes only completed calendar transcripts to the managed workspace", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-publish"));
    mocks.execute.mockResolvedValue([{ value_json: '"workspace-1"' }]);
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "ok",
      data: {
        id: "meeting-1",
        calendar_event_key: "google-event|2026-10-15T15:00:00.000Z",
        ended_at: "2026-10-15T15:30:00.000Z",
        transcripts: [{ ended_at_ms: 1792092600000, text: "Decision made." }],
      },
    });
    mocks.fetch.mockImplementation(
      async (input: string) =>
        new Response(
          JSON.stringify(
            input.endsWith("/v1/cloud-api/settings")
              ? { enabled: true, updated_at: null }
              : { share_id: "share-1" },
          ),
          { status: 200 },
        ),
    );

    await syncCloudApiSnapshot("meeting-1");

    expect(mocks.fetch).toHaveBeenCalledWith(
      "https://api.anarlog.test/v1/sync-snapshots/meeting-1/workspace",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ workspace_id: "workspace-1" }),
      }),
    );

    mocks.fetch.mockClear();
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "ok",
      data: {
        id: "meeting-2",
        calendar_event_key: "google-event-2|2026-10-15T16:00:00.000Z",
        ended_at: "2026-10-15T16:30:00.000Z",
        transcripts: [{ ended_at_ms: null, text: "Partial text" }],
      },
    });
    await syncCloudApiSnapshot("meeting-2");
    expect(
      mocks.fetch.mock.calls.some(([url]) =>
        String(url).includes("/v1/sync-snapshots/meeting-2"),
      ),
    ).toBe(false);

    mocks.fetch.mockClear();
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "ok",
      data: {
        id: "personal-huddle",
        ended_at: "2026-10-15T17:30:00.000Z",
        transcripts: [{ ended_at_ms: 1792096200000, text: "Personal." }],
      },
    });
    await syncCloudApiSnapshot("personal-huddle");
    expect(
      mocks.fetch.mock.calls.some(([url]) =>
        String(url).includes("/v1/sync-snapshots/personal-huddle"),
      ),
    ).toBe(false);
  });

  it("sends private Meet lookup inputs only when registering the workspace candidate", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-overlap"));
    mocks.execute.mockImplementation(async (sql: string) =>
      sql.includes("managed_app_settings")
        ? [{ value_json: '"workspace-1"' }]
        : [
            {
              evidence: JSON.stringify({
                connection_id: "meet-secret",
                conference_record: "conferenceRecords/record-1",
              }),
            },
          ],
    );
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "ok",
      data: {
        id: "meeting-1",
        calendar_event_key: "google:9:meeting-1",
        ended_at: "2026-10-15T15:30:00.000Z",
        transcripts: [
          {
            started_at_ms: 10_000,
            ended_at_ms: 30_000,
            text: "A useful transcript.",
          },
        ],
        shared_transcript_choice: {
          conference_record: null,
          meet_attendance_status: "unknown",
          recorded_seconds: 20,
          verified_overlap_seconds: null,
          transcript_characters: 20,
          transcript_coverage_seconds: 18,
        },
      },
    });
    mocks.fetch.mockResolvedValue(
      new Response(JSON.stringify({ enabled: true, updated_at: null }), {
        status: 200,
      }),
    );

    await syncCloudApiSnapshot("meeting-1");
    const upload = mocks.fetch.mock.calls.find(([url]) =>
      String(url).endsWith("/v1/sync-snapshots/meeting-1"),
    );
    const body = JSON.parse(String(upload?.[1]?.body));
    expect(body.shared_transcript_choice).toMatchObject({
      conference_record: null,
      meet_attendance_status: "unknown",
      verified_overlap_seconds: null,
    });
    expect(JSON.stringify(body)).not.toContain("meet-secret");
    const share = mocks.fetch.mock.calls.find(([url]) =>
      String(url).endsWith("/v1/sync-snapshots/meeting-1/workspace"),
    );
    expect(JSON.parse(String(share?.[1]?.body))).toEqual({
      workspace_id: "workspace-1",
      meet_attendance: {
        connection_id: "meet-secret",
        conference_record: "conferenceRecords/record-1",
      },
    });
  });

  it("retries a failed candidate registration instead of marking it uploaded", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-dedupe"));
    mocks.execute.mockResolvedValue([{ value_json: '"workspace-1"' }]);
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "ok",
      data: {
        id: "meeting-1",
        calendar_event_key: "same-calendar-occurrence",
        ended_at: "2026-10-15T15:30:00.000Z",
        transcripts: [{ ended_at_ms: 1792092600000, text: "Complete." }],
      },
    });
    mocks.fetch.mockImplementation(async (input: string) => {
      if (input.endsWith("/workspace")) {
        return new Response(
          JSON.stringify({
            error: { code: "conflict", message: "already shared" },
          }),
          { status: 409 },
        );
      }
      return new Response(JSON.stringify({ enabled: true, updated_at: null }), {
        status: 200,
      });
    });

    await expect(syncCloudApiSnapshot("meeting-1")).rejects.toMatchObject({
      code: "conflict",
    });
    expect(
      mocks.fetch.mock.calls.some(([url]) =>
        String(url).endsWith("/workspace"),
      ),
    ).toBe(true);
  });

  it("enables Cloud API once when managed workspace sharing is configured", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-managed"));
    mocks.execute.mockResolvedValue([{ value_json: '"workspace-1"' }]);
    mocks.fetch.mockImplementation(
      async (input: string, init?: RequestInit) => {
        if (input.endsWith("/v1/cloud-api/settings")) {
          return new Response(
            JSON.stringify({
              enabled: init?.method === "PUT",
              updated_at: null,
            }),
            { status: 200 },
          );
        }
        return new Response("{}", { status: 200 });
      },
    );

    await syncCloudApiSnapshot("meeting-1");
    await syncCloudApiSnapshot("meeting-2");

    expect(
      mocks.fetch.mock.calls.filter(
        ([url, init]) =>
          String(url).endsWith("/v1/cloud-api/settings") &&
          init?.method === "PUT",
      ),
    ).toHaveLength(1);
  });

  it("does not upload a local snapshot after the signed-in account changes", async () => {
    mocks.getSession
      .mockResolvedValueOnce(authSession("user-a"))
      .mockResolvedValueOnce(authSession("user-a"))
      .mockResolvedValueOnce(authSession("user-a"))
      .mockResolvedValueOnce(authSession("user-a"))
      .mockResolvedValueOnce(authSession("user-b"));

    await expect(syncCloudApiSnapshot("meeting-1")).rejects.toEqual(
      expect.objectContaining<Partial<CloudApiClientError>>({
        code: "unauthorized",
      }),
    );

    expect(mocks.fetch).toHaveBeenCalledTimes(1);
    expect(mocks.getCloudSnapshot).toHaveBeenCalledWith("meeting-1");
  });

  it("continues backfill after a meeting cannot be uploaded", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-backfill"));
    mocks.listCloudSnapshotIds.mockResolvedValue({
      status: "ok",
      data: ["meeting-too-large", "meeting-2"],
    });
    mocks.getCloudSnapshot.mockImplementation(async (sessionId: string) => ({
      status: "ok",
      data: { id: sessionId, transcripts: [] },
    }));
    mocks.fetch.mockImplementation(async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith("/v1/cloud-api/settings")) {
        return new Response(
          JSON.stringify({ enabled: true, updated_at: null }),
          { status: 200 },
        );
      }
      if (url.includes("meeting-too-large")) {
        return new Response(
          JSON.stringify({
            error: { code: "invalid_request", message: "Snapshot too large" },
          }),
          { status: 400 },
        );
      }
      return new Response(null, { status: 204 });
    });
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    await expect(backfillCloudApiSnapshots()).resolves.toBe(1);

    expect(mocks.getCloudSnapshot).toHaveBeenCalledWith("meeting-too-large");
    expect(mocks.getCloudSnapshot).toHaveBeenCalledWith("meeting-2");
    expect(consoleError).toHaveBeenCalledWith(
      "[cloud-api] background sync failed",
      "CloudApiClientError [invalid_request]: Snapshot too large",
    );
  });

  it("uploads snapshots one at a time across meetings", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-upload-queue"));
    mocks.getCloudSnapshot.mockImplementation(async (sessionId: string) => ({
      status: "ok",
      data: { id: sessionId, transcripts: [] },
    }));

    await getCloudApiSettings();

    const pendingUploads: Array<() => void> = [];
    mocks.fetch.mockImplementation((input: string | URL | Request) => {
      if (!String(input).includes("/v1/sync-snapshots/")) {
        return Promise.resolve(new Response(null, { status: 200 }));
      }
      return new Promise<Response>((resolve) => {
        pendingUploads.push(() => resolve(new Response(null, { status: 204 })));
      });
    });

    const first = syncCloudApiSnapshot("meeting-1");
    const second = syncCloudApiSnapshot("meeting-2");

    await vi.waitFor(() => expect(pendingUploads).toHaveLength(1));
    expect(
      mocks.fetch.mock.calls.filter(([input]) =>
        String(input).includes("/v1/sync-snapshots/"),
      ),
    ).toHaveLength(1);

    pendingUploads[0]?.();
    await vi.waitFor(() => expect(pendingUploads).toHaveLength(2));
    pendingUploads[1]?.();

    await expect(Promise.all([first, second])).resolves.toEqual([
      undefined,
      undefined,
    ]);
    expect(
      mocks.fetch.mock.calls
        .filter(([input]) => String(input).includes("/v1/sync-snapshots/"))
        .map(([input]) => String(input).split("/").pop()),
    ).toEqual(["meeting-1", "meeting-2"]);
  });

  it("skips an upload deleted while waiting on the request queue", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-delete-queue"));
    mocks.getCloudSnapshot.mockImplementation(async (sessionId: string) => ({
      status: "ok",
      data: { id: sessionId, transcripts: [] },
    }));

    await getCloudApiSettings();

    let releaseBlockingUpload: (() => void) | undefined;
    mocks.fetch.mockImplementation(
      (input: string | URL | Request, init?: RequestInit) => {
        if (
          String(input).endsWith("/v1/sync-snapshots/meeting-blocking") &&
          init?.method === "PUT"
        ) {
          return new Promise<Response>((resolve) => {
            releaseBlockingUpload = () =>
              resolve(new Response(null, { status: 204 }));
          });
        }
        return Promise.resolve(new Response(null, { status: 204 }));
      },
    );

    const blockingSync = syncCloudApiSnapshot("meeting-blocking");
    await vi.waitFor(() => expect(releaseBlockingUpload).toBeDefined());

    const deletedSync = syncCloudApiSnapshot("meeting-deleted-in-queue");
    await vi.waitFor(() =>
      expect(mocks.getCloudSnapshot).toHaveBeenCalledWith(
        "meeting-deleted-in-queue",
      ),
    );
    localStorage.setItem(
      "anarlog.cloud-api-pending.v1.user-delete-queue",
      JSON.stringify({
        upserts: ["meeting-deleted-in-queue"],
        deletes: [],
      }),
    );
    let resolveDeleteSession:
      | ((session: ReturnType<typeof authSession>) => void)
      | undefined;
    mocks.getSession.mockReturnValueOnce(
      new Promise<ReturnType<typeof authSession>>((resolve) => {
        resolveDeleteSession = resolve;
      }),
    );
    const sessionCallsBeforeDelete = mocks.getSession.mock.calls.length;
    deleteCloudApiSnapshotBestEffort("meeting-deleted-in-queue");
    await vi.waitFor(() =>
      expect(mocks.getSession.mock.calls.length).toBeGreaterThan(
        sessionCallsBeforeDelete,
      ),
    );

    releaseBlockingUpload?.();
    await Promise.all([blockingSync, deletedSync]);
    expect(
      JSON.parse(
        localStorage.getItem(
          "anarlog.cloud-api-pending.v1.user-delete-queue",
        ) ?? "{}",
      ),
    ).toEqual({
      upserts: ["meeting-deleted-in-queue"],
      deletes: [],
    });

    resolveDeleteSession?.(authSession("user-delete-queue"));
    await vi.waitFor(() =>
      expect(mocks.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/v1/sync-snapshots/meeting-deleted-in-queue"),
        expect.objectContaining({ method: "DELETE" }),
      ),
    );

    expect(
      mocks.fetch.mock.calls
        .filter(([input]) =>
          String(input).includes("/v1/sync-snapshots/meeting-deleted-in-queue"),
        )
        .map(([, init]) => (init as RequestInit).method),
    ).toEqual(["DELETE"]);
  });

  it("queues transient local snapshot failures for retry", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-local-snapshot"));
    mocks.listCloudSnapshotIds.mockResolvedValue({
      status: "ok",
      data: ["meeting-locked"],
    });
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "error",
      error: "database is locked",
    });
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    await expect(backfillCloudApiSnapshots()).rejects.toEqual(
      expect.objectContaining<Partial<CloudApiClientError>>({
        code: "local_snapshot_unavailable",
      }),
    );

    expect(
      JSON.parse(
        localStorage.getItem(
          "anarlog.cloud-api-pending.v1.user-local-snapshot",
        ) ?? "{}",
      ),
    ).toEqual({
      upserts: ["meeting-locked"],
      deletes: [],
    });
    expect(consoleError).toHaveBeenCalledOnce();
  });

  it("keeps deletion dominant over a stale failed upload", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-delete-race"));
    mocks.listCloudSnapshotIds.mockResolvedValue({
      status: "ok",
      data: ["meeting-deleted"],
    });
    let resolveSnapshot:
      | ((result: { status: "error"; error: string }) => void)
      | undefined;
    mocks.getCloudSnapshot.mockReturnValue(
      new Promise((resolve) => {
        resolveSnapshot = resolve;
      }),
    );
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    syncCloudApiSnapshotBestEffort("meeting-deleted");
    await vi.waitFor(() =>
      expect(mocks.getCloudSnapshot).toHaveBeenCalledWith("meeting-deleted"),
    );

    deleteCloudApiSnapshotBestEffort("meeting-deleted");
    await vi.waitFor(() =>
      expect(
        JSON.parse(
          localStorage.getItem(
            "anarlog.cloud-api-pending.v1.user-delete-race",
          ) ?? "{}",
        ).deletes,
      ).toEqual(["meeting-deleted"]),
    );
    expect(
      mocks.fetch.mock.calls.some(
        ([, init]) => (init as RequestInit | undefined)?.method === "DELETE",
      ),
    ).toBe(false);

    resolveSnapshot?.({ status: "error", error: "database is locked" });
    await vi.waitFor(() =>
      expect(mocks.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/v1/sync-snapshots/meeting-deleted"),
        expect.objectContaining({ method: "DELETE" }),
      ),
    );

    await vi.waitFor(() => expect(consoleError).toHaveBeenCalledOnce());

    expect(
      JSON.parse(
        localStorage.getItem("anarlog.cloud-api-pending.v1.user-delete-race") ??
          "{}",
      ),
    ).toEqual({
      upserts: [],
      deletes: [],
    });
  });

  it("runs a delete after every upload already queued for the meeting", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-delete-order"));
    const resolveSnapshots: Array<
      (result: {
        status: "ok";
        data: { id: string; transcripts: never[] };
      }) => void
    > = [];
    mocks.getCloudSnapshot.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSnapshots.push(resolve);
        }),
    );

    const firstSync = syncCloudApiSnapshot("meeting-queued");
    const secondSync = syncCloudApiSnapshot("meeting-queued");
    await vi.waitFor(() =>
      expect(mocks.getCloudSnapshot).toHaveBeenCalledOnce(),
    );

    deleteCloudApiSnapshotBestEffort("meeting-queued");
    resolveSnapshots[0]?.({
      status: "ok",
      data: { id: "meeting-queued", transcripts: [] },
    });
    await vi.waitFor(() =>
      expect(mocks.getCloudSnapshot).toHaveBeenCalledTimes(2),
    );
    resolveSnapshots[1]?.({
      status: "ok",
      data: { id: "meeting-queued", transcripts: [] },
    });

    await Promise.all([firstSync, secondSync]);
    await vi.waitFor(() =>
      expect(mocks.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/v1/sync-snapshots/meeting-queued"),
        expect.objectContaining({ method: "DELETE" }),
      ),
    );
    expect(
      mocks.fetch.mock.calls
        .filter(([url]) => String(url).includes("/v1/sync-snapshots/"))
        .map(([, init]) => (init as RequestInit).method),
    ).toEqual(["DELETE"]);
  });

  it("cancels a scheduled upload when the meeting is deleted", async () => {
    vi.useFakeTimers();
    mocks.getSession.mockResolvedValue(authSession("user-timer-delete"));

    scheduleCloudApiSnapshotSync("meeting-scheduled");
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBe(1);

    deleteCloudApiSnapshotBestEffort("meeting-scheduled");
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(1500);

    expect(mocks.getCloudSnapshot).not.toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("retries a scheduled snapshot after a transient invalid request", async () => {
    vi.useFakeTimers();
    mocks.getSession.mockResolvedValue(authSession("user-scheduled-retry"));
    mocks.fetch.mockImplementation(async (input: string | URL | Request) => {
      if (!String(input).includes("/v1/sync-snapshots/")) {
        return new Response(
          JSON.stringify({ enabled: true, updated_at: null }),
          { status: 200 },
        );
      }
      if (
        mocks.fetch.mock.calls.filter(([url]) =>
          String(url).includes("/v1/sync-snapshots/"),
        ).length === 1
      ) {
        return new Response(
          JSON.stringify({
            error: {
              code: "invalid_request",
              message: "Snapshot rows are still settling",
            },
          }),
          { status: 400 },
        );
      }
      return new Response(null, { status: 204 });
    });
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    scheduleCloudApiSnapshotSync("meeting-scheduled-retry");
    await vi.advanceTimersByTimeAsync(1_500);

    expect(mocks.getCloudSnapshot).toHaveBeenCalledOnce();
    expect(consoleError).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(5_000);

    expect(mocks.getCloudSnapshot).toHaveBeenCalledTimes(2);
    expect(consoleError).not.toHaveBeenCalled();
    consoleError.mockRestore();
    vi.useRealTimers();
  });

  it("retries the latest live snapshot when an immediate sync supersedes a scheduled retry", async () => {
    vi.useFakeTimers();
    mocks.getSession.mockResolvedValue(authSession("user-superseded-retry"));
    mocks.fetch.mockImplementation(async (input: string | URL | Request) => {
      if (!String(input).includes("/v1/sync-snapshots/")) {
        return new Response(
          JSON.stringify({ enabled: true, updated_at: null }),
          { status: 200 },
        );
      }
      const uploadCount = mocks.fetch.mock.calls.filter(([url]) =>
        String(url).includes("/v1/sync-snapshots/"),
      ).length;
      if (uploadCount <= 2) {
        return new Response(
          JSON.stringify({
            error: {
              code: "invalid_request",
              message: "Snapshot rows are still settling",
            },
          }),
          { status: 400 },
        );
      }
      return new Response(null, { status: 204 });
    });
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

    scheduleCloudApiSnapshotSync("meeting-superseded-retry");
    await vi.advanceTimersByTimeAsync(1_500);
    expect(mocks.getCloudSnapshot).toHaveBeenCalledOnce();

    syncCloudApiSnapshotBestEffort("meeting-superseded-retry");
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.getCloudSnapshot).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(5_000);

    expect(mocks.getCloudSnapshot).toHaveBeenCalledTimes(3);
    expect(consoleError).not.toHaveBeenCalled();
    consoleError.mockRestore();
    vi.useRealTimers();
  });

  it("deletes a pending snapshot after its local meeting disappears", async () => {
    mocks.getSession.mockResolvedValue(authSession("user-gone"));
    mocks.getCloudSnapshot.mockResolvedValue({
      status: "error",
      error: "meeting not found",
    });
    mocks.listCloudSnapshotIds.mockResolvedValue({
      status: "ok",
      data: [],
    });
    localStorage.setItem("anarlog.cloud-api-backfill.v1.user-gone", "complete");
    localStorage.setItem(
      "anarlog.cloud-api-pending.v1.user-gone",
      JSON.stringify({ upserts: ["meeting-gone"], deletes: [] }),
    );

    await initializeCloudApiBackfill();

    expect(mocks.fetch).toHaveBeenCalledWith(
      expect.stringContaining("/v1/sync-snapshots/meeting-gone"),
      expect.objectContaining({ method: "DELETE" }),
    );
    expect(
      JSON.parse(
        localStorage.getItem("anarlog.cloud-api-pending.v1.user-gone") ?? "{}",
      ),
    ).toEqual({
      upserts: [],
      deletes: [],
    });
  });

  it("aborts a hung snapshot request and allows a later sync", async () => {
    vi.useFakeTimers();
    try {
      mocks.getSession.mockResolvedValue(authSession("user-timeout"));
      mocks.fetch.mockImplementation(
        (input: string | URL | Request, init?: RequestInit) => {
          if (String(input).endsWith("/v1/cloud-api/settings")) {
            return Promise.resolve(
              new Response(
                JSON.stringify({ enabled: true, updated_at: null }),
                { status: 200 },
              ),
            );
          }
          const signal = init?.signal;
          return new Promise<Response>((_, reject) => {
            signal?.addEventListener("abort", () => reject(signal.reason), {
              once: true,
            });
          });
        },
      );

      const firstSync = syncCloudApiSnapshot("meeting-timeout");
      const timedOut = expect(firstSync).rejects.toEqual(
        expect.objectContaining<Partial<CloudApiClientError>>({
          code: "request_timeout",
        }),
      );
      await vi.advanceTimersByTimeAsync(30_000);
      await timedOut;
      const uploadRequest = mocks.fetch.mock.calls.find(
        ([input]) => !String(input).endsWith("/v1/cloud-api/settings"),
      );
      expect(
        (uploadRequest?.[1] as RequestInit | undefined)?.signal?.aborted,
      ).toBe(true);

      mocks.fetch.mockResolvedValue(new Response(null, { status: 204 }));
      await expect(
        syncCloudApiSnapshot("meeting-timeout"),
      ).resolves.toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });
});
