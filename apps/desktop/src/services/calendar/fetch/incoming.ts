import { commands as calendarCommands } from "@anlg/plugin-calendar";
import type { CalendarEvent } from "@anlg/plugin-calendar";

import { createAttendanceSnapshot } from "../attendance";
import type { Ctx } from "../ctx";
import type {
  EventParticipant,
  IncomingEvent,
  IncomingParticipants,
} from "./types";

export class CalendarFetchError extends Error {
  constructor(
    public readonly calendarTrackingId: string,
    public readonly cause: string,
  ) {
    super(
      `Failed to fetch events for calendar ${calendarTrackingId}: ${cause}`,
    );
    this.name = "CalendarFetchError";
  }
}

export async function fetchIncomingEvents(ctx: Ctx): Promise<{
  events: IncomingEvent[];
  participants: IncomingParticipants;
}> {
  const trackingIds = Array.from(ctx.calendarTrackingIdToId.keys());

  const results = await Promise.all(
    trackingIds.map(async (trackingId) => {
      const result = await calendarCommands.listEvents(
        ctx.provider,
        ctx.connectionId,
        {
          calendar_tracking_id: trackingId,
          from: ctx.from.toISOString(),
          to: ctx.to.toISOString(),
        },
      );

      if (result.status === "error") {
        throw new CalendarFetchError(trackingId, result.error);
      }

      return result.data;
    }),
  );

  const calendarEvents = results.flat();
  const observedAt = new Date();
  const events: IncomingEvent[] = [];
  const participants: IncomingParticipants = new Map();

  for (const calendarEvent of calendarEvents) {
    const attendance = createAttendanceSnapshot(
      calendarEvent.attendance,
      observedAt,
    );
    if (attendance?.self_status === "declined") continue;
    const { event, eventParticipants } = normalizeCalendarEvent(
      calendarEvent,
      attendance,
    );
    events.push(event);
    if (!event.is_cancelled) {
      participants.set(event.tracking_id_event, eventParticipants);
    }
  }

  return { events, participants };
}

// Meeting links are fully resolved on the Rust side during provider
// conversion, so no per-event parse IPC happens here.
function normalizeCalendarEvent(
  calendarEvent: CalendarEvent,
  attendance: IncomingEvent["attendance"],
): {
  event: IncomingEvent;
  eventParticipants: EventParticipant[];
} {
  const eventParticipants: EventParticipant[] = [];

  if (calendarEvent.organizer) {
    eventParticipants.push({
      name: calendarEvent.organizer.name ?? undefined,
      email: calendarEvent.organizer.email ?? undefined,
      is_organizer: true,
      is_current_user: calendarEvent.organizer.is_current_user,
    });
  }

  const organizerEmail = calendarEvent.organizer?.email?.toLowerCase();

  for (const attendee of calendarEvent.attendees) {
    if (attendee.role === "nonparticipant") continue;
    if (organizerEmail && attendee.email?.toLowerCase() === organizerEmail)
      continue;
    eventParticipants.push({
      name: attendee.name ?? undefined,
      email: attendee.email ?? undefined,
      is_organizer: false,
      is_current_user: attendee.is_current_user,
    });
  }

  return {
    event: {
      tracking_id_event: calendarEvent.id,
      tracking_id_calendar: calendarEvent.calendar_id,
      legacy_tracking_ids: calendarEvent.legacy_ids ?? [],
      is_cancelled: calendarEvent.status === "cancelled",
      title: calendarEvent.title,
      started_at: calendarEvent.started_at,
      ended_at: calendarEvent.ended_at,
      location: calendarEvent.location ?? undefined,
      meeting_link: calendarEvent.meeting_link ?? undefined,
      description: calendarEvent.description ?? undefined,
      recurrence_series_id: calendarEvent.recurring_event_id ?? undefined,
      shared_occurrence_key: googleSharedOccurrenceKey(calendarEvent),
      has_recurrence_rules: calendarEvent.has_recurrence_rules,
      is_all_day: calendarEvent.is_all_day,
      attendance,
    },
    eventParticipants,
  };
}

export function googleSharedOccurrenceKey(event: CalendarEvent): string | undefined {
  if (event.provider !== "google") return undefined;
  const uid = event.external_id.trim();
  if (!uid) return undefined;
  let occurrence = "";
  if (event.recurring_event_id || event.has_recurrence_rules) {
    let original: { date?: unknown; dateTime?: unknown } | undefined;
    try {
      const raw = JSON.parse(event.raw) as { originalStartTime?: typeof original };
      original = raw.originalStartTime;
    } catch {
      return undefined;
    }
    if (typeof original?.dateTime === "string") {
      const timestamp = Date.parse(original.dateTime);
      if (!Number.isFinite(timestamp)) return undefined;
      occurrence = `:T${new Date(timestamp).toISOString()}`;
    } else if (
      typeof original?.date === "string" &&
      /^\d{4}-\d{2}-\d{2}$/.test(original.date)
    ) {
      occurrence = `:D${original.date}`;
    } else {
      return undefined;
    }
  }
  const key = `google:${uid.length}:${uid}${occurrence}`;
  return new TextEncoder().encode(key).length <= 512 ? key : undefined;
}
