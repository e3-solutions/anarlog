import { beforeEach, describe, expect, test, vi } from "vitest";

const calendarCommands = vi.hoisted(() => ({
  listEvents: vi.fn(),
}));

vi.mock("@anlg/plugin-calendar", () => ({
  commands: calendarCommands,
}));

import type { Ctx } from "../ctx";
import { fetchIncomingEvents, googleSharedOccurrenceKey } from "./incoming";

test("groups the same Google occurrence across accounts and preserves moved recurrences", () => {
  const base = {
    provider: "google",
    external_id: "series@example.com",
    recurring_event_id: "series",
    has_recurrence_rules: true,
    raw: JSON.stringify({
      originalStartTime: { dateTime: "2026-06-01T10:00:00-04:00" },
    }),
    started_at: "2026-06-01T15:00:00Z",
  } as Parameters<typeof googleSharedOccurrenceKey>[0];
  const key = googleSharedOccurrenceKey(base);
  expect(key).toBe("google:18:series@example.com:T2026-06-01T14:00:00.000Z");
  expect(
    googleSharedOccurrenceKey({ ...base, started_at: "2026-06-01T16:00:00Z" }),
  ).toBe(key);
  expect(
    googleSharedOccurrenceKey({
      ...base,
      raw: JSON.stringify({
        originalStartTime: { dateTime: "2026-06-08T10:00:00-04:00" },
      }),
    }),
  ).not.toBe(key);
  expect(
    googleSharedOccurrenceKey({ ...base, external_id: "" }),
  ).toBeUndefined();
});

const ctx: Ctx = {
  provider: "google",
  connectionId: "conn-1",
  from: new Date("2026-06-01T00:00:00.000Z"),
  to: new Date("2026-06-02T00:00:00.000Z"),
  calendarIds: new Set(["cal-1"]),
  calendarTrackingIdToId: new Map([["primary", "cal-1"]]),
  calendars: [{ id: "cal-1", tracking_id_calendar: "primary" }],
};

describe("fetchIncomingEvents", () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  test("carries exact legacy aliases and cancellation through the provider boundary", async () => {
    calendarCommands.listEvents.mockResolvedValue({
      status: "success",
      data: [
        {
          id: "canonical",
          legacy_ids: ["old-id"],
          calendar_id: "primary",
          title: "Canceled",
          status: "cancelled",
          started_at: "2026-06-01T10:00:00Z",
          ended_at: "2026-06-01T11:00:00Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
        },
      ],
    });
    const result = await fetchIncomingEvents(ctx);
    expect(result.events[0]).toMatchObject({
      tracking_id_event: "canonical",
      legacy_tracking_ids: ["old-id"],
      is_cancelled: true,
    });
    expect(result.participants.size).toBe(0);
  });

  test("records an empty participant list so stale auto mappings are removed", async () => {
    calendarCommands.listEvents.mockResolvedValue({
      status: "success",
      data: [
        {
          id: "event-1",
          calendar_id: "primary",
          title: "No attendees",
          started_at: "2026-06-01T10:00:00.000Z",
          ended_at: "2026-06-01T11:00:00.000Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
        },
      ],
    });

    const result = await fetchIncomingEvents(ctx);

    expect(result.events).toHaveLength(1);
    expect(result.participants.has("event-1")).toBe(true);
    expect(result.participants.get("event-1")).toEqual([]);
  });

  test("passes through the meeting link resolved during provider conversion", async () => {
    const meetingLink = "https://meet.google.com/abc-defg-hij";
    calendarCommands.listEvents.mockResolvedValue({
      status: "success",
      data: [
        {
          id: "event-1",
          calendar_id: "primary",
          title: "Customer call",
          description: "https://cal.com/customer-call/reschedule",
          location: "Conference room 4",
          meeting_link: meetingLink,
          started_at: "2026-06-01T10:00:00.000Z",
          ended_at: "2026-06-01T11:00:00.000Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
        },
      ],
    });

    const result = await fetchIncomingEvents(ctx);

    expect(result.events[0]?.meeting_link).toBe(meetingLink);
  });

  test("stores normalized attendance evidence with one observation time", async () => {
    calendarCommands.listEvents.mockResolvedValue({
      status: "success",
      data: [
        {
          id: "event-1",
          calendar_id: "primary",
          title: "Accepted",
          started_at: "2026-06-01T10:00:00.000Z",
          ended_at: "2026-06-01T11:00:00.000Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
          attendance: {
            self_status: "accepted",
            roster_status: "complete",
            others: {
              accepted: 1,
              tentative: 0,
              pending: 0,
              declined: 2,
              unknown: 0,
            },
          },
        },
        {
          id: "event-2",
          calendar_id: "primary",
          title: "Pending",
          started_at: "2026-06-01T12:00:00.000Z",
          ended_at: "2026-06-01T13:00:00.000Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
          attendance: {
            self_status: "pending",
            roster_status: "incomplete",
            others: {
              accepted: 0,
              tentative: 0,
              pending: 0,
              declined: 1,
              unknown: 0,
            },
          },
        },
      ],
    });

    const result = await fetchIncomingEvents(ctx);

    expect(result.events).toHaveLength(2);
    expect(result.events[0]?.attendance).toMatchObject({
      version: 1,
      self_status: "accepted",
      others: { accepted: 1, declined: 2 },
    });
    expect(result.events[0]?.attendance?.observed_at).toBe(
      result.events[1]?.attendance?.observed_at,
    );
  });

  test("omits self-declined events using normalized provider attendance", async () => {
    calendarCommands.listEvents.mockResolvedValue({
      status: "success",
      data: [
        {
          id: "event-1",
          calendar_id: "primary",
          title: "Declined",
          started_at: "2026-06-01T10:00:00.000Z",
          ended_at: "2026-06-01T11:00:00.000Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
          attendance: {
            self_status: "declined",
            roster_status: "complete",
            others: {
              accepted: 1,
              tentative: 0,
              pending: 0,
              declined: 0,
              unknown: 0,
            },
          },
        },
      ],
    });

    const result = await fetchIncomingEvents(ctx);

    expect(result.events).toEqual([]);
    expect(result.participants.size).toBe(0);
  });

  test("keeps group-declined events visible for scheduler policy", async () => {
    calendarCommands.listEvents.mockResolvedValue({
      status: "success",
      data: [
        {
          id: "event-1",
          calendar_id: "primary",
          title: "Everyone declined",
          started_at: "2026-06-01T10:00:00.000Z",
          ended_at: "2026-06-01T11:00:00.000Z",
          attendees: [],
          organizer: null,
          has_recurrence_rules: false,
          is_all_day: false,
          attendance: {
            self_status: "organizer",
            roster_status: "complete",
            others: {
              accepted: 0,
              tentative: 0,
              pending: 0,
              declined: 2,
              unknown: 0,
            },
          },
        },
      ],
    });

    const result = await fetchIncomingEvents(ctx);

    expect(result.events).toHaveLength(1);
    expect(result.events[0]?.attendance?.others.declined).toBe(2);
  });
});
