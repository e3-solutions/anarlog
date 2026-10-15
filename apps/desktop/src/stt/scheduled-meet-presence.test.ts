import { afterEach, beforeEach, expect, test, vi } from "vitest";

import {
  googleMeetCode,
  hasJoinedScheduledMeet,
  isE3CalendarId,
} from "./scheduled-meet-presence";

const mocks = vi.hoisted(() => ({ listConnections: vi.fn() }));

vi.mock("@anlg/api-client", () => ({
  listConnections: mocks.listConnections,
}));
vi.mock("@anlg/api-client/client", () => ({
  createClient: () => ({}),
}));

const start = new Date("2026-05-15T12:00:00.000Z").getTime();
const row = {
  id: "event-1",
  started_at: new Date(start).toISOString(),
  ended_at: new Date(start + 30 * 60_000).toISOString(),
  meeting_link: "https://meet.google.com/abc-defg-hij",
  tracking_id_event: "tracking-1",
  recurrence_series_id: "",
  attendance_json: null,
  calendar_connection_id: "calendar-1",
  calendar_provider: "google",
  calendar_enabled: 1,
  calendar_deleted_at: null,
  calendar_tracking_id: "priyal@e3group.ai",
};
const headers = { Authorization: "Bearer test" };

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(start + 60_000);
  mocks.listConnections.mockReset().mockResolvedValue({
    data: {
      connections: [
        {
          integration_id: "google-calendar",
          connection_id: "calendar-1",
          account_identity: "Priyal@e3group.ai",
          status: "connected",
        },
        {
          integration_id: "google-meet",
          connection_id: "meet-1",
          account_identity: "priyal@e3group.ai",
          status: "connected",
        },
      ],
    },
    error: null,
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

test.each([
  ["https://meet.google.com/abc-defg-hij", "abc-defg-hij"],
  ["https://meet.google.com/long-code-example?authuser=0", "long-code-example"],
  ["https://meet.google.com/lookup/abc-defg-hij", null],
  ["https://meet.google.com.evil.com/abc-defg-hij", null],
  ["https://zoom.us/j/123", null],
])("extracts a safe Meet code from %s", (link, expected) => {
  expect(googleMeetCode(link)).toBe(expected);
});

test("accepts employee primary calendar IDs from the two E3 domains", () => {
  expect(isE3CalendarId("Priyal@e3group.ai")).toBe(true);
  expect(isE3CalendarId("priyal@coreedgesolution.com")).toBe(true);
  expect(isE3CalendarId("shared-team@other.com")).toBe(false);
  expect(isE3CalendarId("secondary-calendar-id")).toBe(false);
});

test("requires active presence for the matching Calendar and Meet account", async () => {
  const fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: async () => ({
      present: true,
      account_email: "PRIYAL@E3GROUP.AI",
      conference_start_time: new Date(start - 2 * 60_000).toISOString(),
      meeting_uri: row.meeting_link,
    }),
  });
  vi.stubGlobal("fetch", fetch);

  await expect(hasJoinedScheduledMeet(row, headers)).resolves.toBe(true);
  expect(fetch).toHaveBeenCalledWith(
    expect.stringContaining("/nango/google-meet/presence"),
    expect.objectContaining({
      body: JSON.stringify({
        connection_id: "meet-1",
        meeting_code: "abc-defg-hij",
      }),
    }),
  );
});

test("does not accept another account or an earlier conference", async () => {
  const fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: async () => ({
      present: true,
      account_email: "other@e3group.ai",
      conference_start_time: new Date(start - 2 * 60_000).toISOString(),
      meeting_uri: row.meeting_link,
    }),
  });
  vi.stubGlobal("fetch", fetch);
  await expect(hasJoinedScheduledMeet(row, headers)).resolves.toBe(false);

  fetch.mockResolvedValue({
    ok: true,
    json: async () => ({
      present: true,
      account_email: "priyal@e3group.ai",
      conference_start_time: new Date(start - 30 * 60_000).toISOString(),
      meeting_uri: row.meeting_link,
    }),
  });
  await expect(hasJoinedScheduledMeet(row, headers)).resolves.toBe(false);
});

test("rejects a coworker's calendar even when it is shared to the employee", async () => {
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  await expect(
    hasJoinedScheduledMeet(
      { ...row, calendar_tracking_id: "coworker@e3group.ai" },
      headers,
    ),
  ).resolves.toBe(false);
  expect(fetch).not.toHaveBeenCalled();
});

test("fails closed on ambiguous Meet connections and provider errors", async () => {
  mocks.listConnections.mockResolvedValueOnce({
    data: {
      connections: [
        {
          integration_id: "google-calendar",
          connection_id: "calendar-1",
          account_identity: "priyal@e3group.ai",
        },
        ...["meet-1", "meet-2"].map((connection_id) => ({
          integration_id: "google-meet",
          connection_id,
          account_identity: "priyal@e3group.ai",
        })),
      ],
    },
  });
  const fetch = vi.fn().mockResolvedValue({ ok: false, status: 403 });
  vi.stubGlobal("fetch", fetch);
  await expect(hasJoinedScheduledMeet(row, headers)).resolves.toBe(false);
  expect(fetch).not.toHaveBeenCalled();
  await expect(hasJoinedScheduledMeet(row, headers)).rejects.toThrow(
    "Meet presence check failed: 403",
  );
});
