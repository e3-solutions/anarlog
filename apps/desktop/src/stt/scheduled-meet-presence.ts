import { listConnections } from "@anlg/api-client";
import { createClient } from "@anlg/api-client/client";
import { parseEventInstant } from "@anlg/utils";

import type { ScheduledMeetingRow } from "./scheduled-auto-start";

import { env } from "~/env";

const E3_EMAIL_DOMAINS = new Set(["e3group.ai", "coreedgesolution.com"]);

export function isE3CalendarId(value: string | undefined): boolean {
  const email = value?.trim().toLowerCase();
  if (!email) return false;
  const parts = email.split("@");
  return parts.length === 2 && E3_EMAIL_DOMAINS.has(parts[1]);
}

export function googleMeetCode(link: string): string | null {
  try {
    const url = new URL(link);
    if (
      url.protocol !== "https:" ||
      url.hostname !== "meet.google.com" ||
      url.username ||
      url.password ||
      url.port
    ) {
      return null;
    }
    const match = /^\/([a-z]+-[a-z]+-[a-z]+)\/?$/.exec(url.pathname);
    return match?.[1] && match[1].length <= 128 ? match[1] : null;
  } catch {
    return null;
  }
}

export function isGoogleMeetLink(link: string): boolean {
  try {
    return new URL(link).hostname === "meet.google.com";
  } catch {
    return false;
  }
}

export async function resolveScheduledMeetConnection(
  row: ScheduledMeetingRow,
  headers: Record<string, string> | null,
): Promise<{ connectionId: string; email: string } | null> {
  if (
    !googleMeetCode(row.meeting_link) ||
    !headers ||
    !row.calendar_connection_id ||
    row.calendar_provider !== "google" ||
    !row.calendar_enabled ||
    row.calendar_deleted_at ||
    !isE3CalendarId(row.calendar_tracking_id)
  )
    return null;

  const signal = AbortSignal.timeout(25_000);
  const client = createClient({ baseUrl: env.VITE_API_URL, headers });
  const { data, error } = await listConnections({ client, signal });
  if (error || !data)
    throw new Error("Could not check connected Google accounts");

  const calendar = data.connections.find(
    (connection) =>
      connection.integration_id === "google-calendar" &&
      connection.connection_id === row.calendar_connection_id &&
      connection.status !== "reconnect_required",
  );
  const email = calendar?.account_identity?.trim().toLowerCase();
  if (!email || row.calendar_tracking_id?.trim().toLowerCase() !== email)
    return null;

  const meetConnections = data.connections.filter(
    (connection) =>
      connection.integration_id === "google-meet" &&
      connection.status !== "reconnect_required" &&
      connection.account_identity?.trim().toLowerCase() === email,
  );
  if (meetConnections.length !== 1) return null;

  return { connectionId: meetConnections[0].connection_id, email };
}

export async function hasJoinedScheduledMeet(
  row: ScheduledMeetingRow,
  headers: Record<string, string> | null,
  resolvedConnection?: { connectionId: string; email: string },
): Promise<boolean> {
  const meetingCode = googleMeetCode(row.meeting_link);
  if (!meetingCode || !headers) return false;
  const connection =
    resolvedConnection ?? (await resolveScheduledMeetConnection(row, headers));
  if (!connection) return false;

  const response = await fetch(
    `${env.VITE_API_URL.replace(/\/$/, "")}/nango/google-meet/presence`,
    {
      method: "POST",
      headers: { ...headers, "Content-Type": "application/json" },
      body: JSON.stringify({
        connection_id: connection.connectionId,
        meeting_code: meetingCode,
      }),
      signal: AbortSignal.timeout(15_000),
    },
  );
  if (!response.ok)
    throw new Error(`Meet presence check failed: ${response.status}`);

  const result: unknown = await response.json();
  if (!result || typeof result !== "object") return false;
  const presence = result as Record<string, unknown>;
  if (
    presence.present !== true ||
    typeof presence.account_email !== "string" ||
    presence.account_email.trim().toLowerCase() !== connection.email ||
    typeof presence.conference_start_time !== "string" ||
    typeof presence.meeting_uri !== "string" ||
    googleMeetCode(presence.meeting_uri) !== meetingCode
  ) {
    return false;
  }

  const conferenceStart = Date.parse(presence.conference_start_time);
  const eventStart = parseEventInstant(row.started_at)?.getTime();
  const eventEnd = parseEventInstant(row.ended_at)?.getTime();
  const now = Date.now();
  return (
    Number.isFinite(conferenceStart) &&
    eventStart !== undefined &&
    eventEnd !== undefined &&
    conferenceStart >= eventStart - 15 * 60_000 &&
    conferenceStart <= now + 30_000 &&
    conferenceStart < eventEnd
  );
}
