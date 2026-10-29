import { beforeEach, describe, expect, it, vi } from "vitest";

const { execute } = vi.hoisted(() => ({ execute: vi.fn() }));
vi.mock("~/db", () => ({ liveQueryClient: { execute } }));

import type { AutomaticMeeting } from "./automatic-meeting";

const meeting: AutomaticMeeting = {
  key: "googleMeet:chrome:12:tab:23",
  app: { id: "com.google.Chrome", name: "Chrome" },
  pid: 12,
  callIdentity: "https://meet.google.com/abc-defg-hij:tab:23",
  platform: "googleMeet",
  title: "Google Meet",
};
const event = {
  id: "local-event",
  calendar_id: "calendar-1",
  tracking_id_event: "series-occurrence",
  started_at: "2026-10-27T10:00:00.000Z",
  meeting_link: "https://meet.google.com/abc-defg-hij?authuser=1",
};

beforeEach(() => {
  localStorage.clear();
  vi.resetModules();
  execute.mockReset().mockResolvedValue([]);
});

describe("recording exclusions", () => {
  it("holds an ad-hoc room across polling, rejoin, background and restart until explicit resume", async () => {
    let policy = await import("./recording-exclusions");
    const occurrence = await policy.resolveRecordingOccurrence("alice", {
      meeting,
    });
    policy.excludeRecordingOccurrence("alice", occurrence, [
      "captured-session",
    ]);
    policy = await import("./recording-exclusions");
    const rejoin = await policy.resolveRecordingOccurrence("alice", {
      meeting: {
        ...meeting,
        key: "different-process-tab",
        pid: 99,
        callIdentity: "https://meet.google.com/abc-defg-hij:window:abc",
      },
    });
    expect(policy.isRecordingExcluded("alice", rejoin)).toBe(true);
    vi.resetModules();
    policy = await import("./recording-exclusions");
    const restored = await policy.resolveRecordingOccurrence("alice", {
      sessionId: "captured-session",
    });
    expect(policy.isRecordingExcluded("alice", restored)).toBe(true);
    policy.resumeRecordingOccurrence("alice", restored, "resumed-new-session");
    expect(policy.isRecordingExcluded("alice", rejoin)).toBe(false);
    expect(policy.isRecordingSessionExcluded("alice", "captured-session")).toBe(
      true,
    );
    expect(
      policy.isRecordingSessionExcluded("alice", "resumed-new-session"),
    ).toBe(false);
  });

  it("isolates accounts and recurring calendar occurrences even when they share a room and tracking ID", async () => {
    const policy = await import("./recording-exclusions");
    const excluded = await policy.resolveRecordingOccurrence("alice", {
      event,
    });
    policy.excludeRecordingOccurrence("alice", excluded);
    const next = await policy.resolveRecordingOccurrence("alice", {
      event: { ...event, started_at: "2026-11-03T10:00:00.000Z" },
    });
    expect(policy.isRecordingExcluded("alice", excluded)).toBe(true);
    expect(policy.isRecordingExcluded("bob", excluded)).toBe(false);
    expect(policy.isRecordingExcluded("alice", next)).toBe(false);
    const otherCalendar = await policy.resolveRecordingOccurrence("alice", {
      event: { ...event, calendar_id: "calendar-2" },
    });
    expect(policy.isRecordingExcluded("alice", otherCalendar)).toBe(false);
  });

  it("keeps ad-hoc room exclusions through late calendar sync and future room reuse until explicit resume", async () => {
    const policy = await import("./recording-exclusions");
    const adHoc = await policy.resolveRecordingOccurrence("alice", { meeting });
    policy.excludeRecordingOccurrence("alice", adHoc);
    execute.mockResolvedValue([
      { ...event, started_at: new Date(Date.now() - 60_000).toISOString() },
    ]);
    const scheduled = await policy.resolveRecordingOccurrence("alice", {
      meeting,
    });
    expect(scheduled.kind).toBe("calendar");
    expect(policy.isRecordingExcluded("alice", scheduled)).toBe(true);
    const later = await policy.resolveRecordingOccurrence("alice", {
      event: {
        ...event,
        started_at: new Date(Date.now() + 86_400_000).toISOString(),
      },
    });
    expect(policy.isRecordingExcluded("alice", later)).toBe(true);
    policy.resumeRecordingOccurrence("alice", later, "resumed-room");
    expect(policy.isRecordingExcluded("alice", later)).toBe(false);
    policy.excludeRecordingOccurrence("alice", adHoc);
    execute.mockResolvedValue([
      event,
      { ...event, id: "competing", calendar_id: "other" },
    ]);
    const ambiguous = await policy.resolveRecordingOccurrence("alice", {
      meeting,
    });
    expect(policy.isRecordingExcluded("alice", ambiguous)).toBe(true);
    const unknown = await policy.resolveRecordingOccurrence("alice", {
      meeting: { ...meeting, callIdentity: null },
    });
    expect(policy.isRecordingExcluded("alice", unknown)).toBe(true);
  });

  it("tombstones all previously bound sessions and keeps them blocked after intentional resume", async () => {
    const policy = await import("./recording-exclusions");
    const occurrence = await policy.resolveRecordingOccurrence("alice", {
      event,
    });
    policy.bindRecordingSession("alice", "queued", occurrence);
    policy.bindRecordingSession("alice", "in-flight", occurrence);
    policy.excludeRecordingOccurrence("alice", occurrence);
    policy.resumeRecordingOccurrence("alice", occurrence);
    vi.resetModules();
    const restored = await import("./recording-exclusions");
    expect(restored.isRecordingSessionExcluded("alice", "queued")).toBe(true);
    expect(restored.isRecordingSessionExcluded("alice", "in-flight")).toBe(
      true,
    );
    expect(restored.isRecordingSessionExcluded("bob", "queued")).toBe(false);
  });

  it("keeps the running privacy hold when persistence fails and reports the failure", async () => {
    const policy = await import("./recording-exclusions");
    const occurrence = await policy.resolveRecordingOccurrence("alice", {
      event,
    });
    const write = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new Error("disk unavailable");
      });
    expect(() =>
      policy.excludeRecordingOccurrence("alice", occurrence, ["session"]),
    ).toThrow("disk unavailable");
    expect(policy.isRecordingExcluded("alice", occurrence)).toBe(true);
    expect(policy.isRecordingSessionExcluded("alice", "session")).toBe(true);
    write.mockRestore();
  });
  it("holds unidentified calls through room and calendar upgrades until explicit resume", async () => {
    const policy = await import("./recording-exclusions");
    const unknown = await policy.resolveRecordingOccurrence("alice", {
      sessionId: "old",
      meeting: { ...meeting, callIdentity: null },
    });
    expect(unknown.kind).toBe("ambiguous");
    policy.excludeRecordingOccurrence("alice", unknown, ["old"]);
    const room = await policy.resolveRecordingOccurrence("alice", { meeting });
    const calendar = await policy.resolveRecordingOccurrence("alice", {
      event,
    });
    expect(policy.isRecordingExcluded("alice", room)).toBe(true);
    expect(policy.isRecordingExcluded("alice", calendar)).toBe(true);
    const unrelated = await policy.resolveRecordingOccurrence("alice", {
      event: { ...event, calendar_id: "other" },
    });
    policy.excludeRecordingOccurrence("alice", unrelated);
    policy.resumeRecordingOccurrence("alice", calendar, "fresh");
    expect(policy.isRecordingExcluded("alice", calendar)).toBe(false);
    expect(policy.isRecordingSessionExcluded("alice", "fresh")).toBe(false);
    expect(policy.isRecordingSessionExcluded("alice", "old")).toBe(true);
    expect(policy.isRecordingExcluded("alice", unrelated)).toBe(true);
  });

  it("keeps the hold and old tombstones when atomic resume cannot be saved", async () => {
    const policy = await import("./recording-exclusions");
    const occurrence = await policy.resolveRecordingOccurrence("alice", {
      event,
    });
    policy.excludeRecordingOccurrence("alice", occurrence, ["old"]);
    const write = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new Error("disk unavailable");
      });
    expect(() =>
      policy.resumeRecordingOccurrence("alice", occurrence, "fresh"),
    ).toThrow("disk unavailable");
    expect(policy.isRecordingExcluded("alice", occurrence)).toBe(true);
    expect(policy.isRecordingSessionExcluded("alice", "old")).toBe(true);
    expect(
      policy.getRecordingOccurrenceSessionIds("alice", occurrence),
    ).not.toContain("fresh");
    write.mockRestore();
    policy.resumeRecordingOccurrence("alice", occurrence, "fresh");
    expect(policy.isRecordingSessionExcluded("alice", "fresh")).toBe(false);
    expect(policy.isRecordingSessionExcluded("alice", "old")).toBe(true);
    expect(() =>
      policy.resumeRecordingOccurrence("alice", occurrence, "old"),
    ).toThrow("new session");
  });
});
