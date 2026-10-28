import type { AutomaticMeeting } from "./automatic-meeting";

export type RecordingOccurrence = {
  key: string;
  room: string | null;
  kind: "calendar" | "room" | "session" | "ambiguous";
  eventId?: string;
};

type EventIdentity = {
  id?: string;
  calendar_id?: string;
  tracking_id?: string;
  tracking_id_event?: string;
  started_at?: string;
  ended_at?: string;
  meeting_link?: string | null;
};
type AccountPolicy = {
  excluded: Record<string, RecordingOccurrence>;
  sessions: Record<string, true>;
  bindings: Record<string, RecordingOccurrence>;
};
type Policy = Record<string, AccountPolicy>;

const STORAGE_KEY = "e3.recording-exclusions.v1";
let revision = 0;
let memory: Policy | undefined;
let savedValue: string | null | undefined;
const listeners = new Set<() => void>();

function read(): Policy {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (memory && raw === savedValue) return memory;
  const parsed: unknown = raw ? JSON.parse(raw) : {};
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Recording exclusions could not be read safely");
  }
  memory = parsed as Policy;
  savedValue = raw;
  return memory;
}

function account(userId: string): AccountPolicy {
  if (!userId) throw new Error("Recording exclusions require an account");
  const policy = read();
  return (policy[userId] ??= { excluded: {}, sessions: {}, bindings: {} });
}

function persist() {
  // Keep the in-memory hold even if disk persistence fails; the caller must
  // still stop capture and show that a restart cannot preserve this choice.
  const value = JSON.stringify(memory);
  try {
    localStorage.setItem(STORAGE_KEY, value);
    savedValue = value;
  } finally {
    revision++;
    for (const listener of listeners) listener();
  }
}

export function subscribeRecordingExclusions(listener: () => void) {
  listeners.add(listener);
  const onStorage = (event: StorageEvent) => {
    if (event.key !== STORAGE_KEY && event.key !== null) return;
    memory = undefined;
    revision++;
    listener();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("storage", onStorage);
  };
}

export function getRecordingExclusionsSnapshot() {
  return revision;
}

export function recordingRoomIdentity(value: string | null | undefined) {
  if (!value) return null;
  const meet = value.match(
    /https:\/\/meet\.google\.com\/[a-z]{3}-[a-z]{4}-[a-z]{3}(?=$|[?:/#])/i,
  );
  if (meet) return meet[0].toLowerCase();
  // Slack native identities identify the Huddle, not its process/window.
  return value.trim() || null;
}

function calendarOccurrence(event: EventIdentity): RecordingOccurrence | null {
  const trackingId = event.tracking_id_event || event.tracking_id || event.id;
  if (!event.calendar_id || !trackingId || !event.started_at) return null;
  const startedAt = new Date(event.started_at).getTime();
  if (!Number.isFinite(startedAt)) return null;
  return {
    key: `calendar:${JSON.stringify([event.calendar_id, trackingId, startedAt])}`,
    room: recordingRoomIdentity(event.meeting_link),
    kind: "calendar",
    ...(event.id ? { eventId: event.id } : {}),
  };
}

export async function resolveRecordingOccurrence(
  userId: string,
  input: {
    meeting?: AutomaticMeeting | null;
    sessionId?: string | null;
    event?: EventIdentity | null;
  },
): Promise<RecordingOccurrence> {
  const policy = account(userId);
  if (input.sessionId && policy.bindings[input.sessionId]) {
    return policy.bindings[input.sessionId]!;
  }
  const explicit = input.event && calendarOccurrence(input.event);
  if (explicit) return explicit;
  const room = recordingRoomIdentity(input.meeting?.callIdentity);
  const { liveQueryClient } = await import("~/db");
  if (input.event?.id) {
    const rows = await liveQueryClient.execute<EventIdentity>(
      `SELECT id, calendar_id, tracking_id_event, started_at, meeting_link
       FROM events WHERE id = ? AND deleted_at IS NULL`,
      [input.event.id],
    );
    const occurrence = rows.length === 1 && calendarOccurrence(rows[0]!);
    if (occurrence) return occurrence;
  }
  if (input.sessionId) {
    const rows = await liveQueryClient.execute<EventIdentity>(
      `SELECT event.id, event.calendar_id, event.tracking_id_event, event.started_at, event.meeting_link
       FROM sessions AS session JOIN events AS event ON (
         event.id = session.event_id OR (
           CASE WHEN json_valid(session.event_json) THEN event.tracking_id_event <> ''
           AND event.tracking_id_event = json_extract(session.event_json, '$.tracking_id')
           AND event.calendar_id = json_extract(session.event_json, '$.calendar_id')
           AND julianday(event.started_at) = julianday(json_extract(session.event_json, '$.started_at'))
           ELSE 0 END
         )
       ) WHERE session.id = ? AND session.deleted_at IS NULL AND event.deleted_at IS NULL`,
      [input.sessionId],
    );
    const occurrence = rows.length === 1 && calendarOccurrence(rows[0]!);
    if (occurrence) return occurrence;
  }
  if (room) {
    const now = new Date().toISOString();
    const rows = await liveQueryClient.execute<EventIdentity>(
      `SELECT id, calendar_id, tracking_id_event, started_at, meeting_link
       FROM events WHERE deleted_at IS NULL AND is_all_day = 0
       AND julianday(started_at) <= julianday(?) AND julianday(ended_at) > julianday(?)`,
      [now, now],
    );
    const matching = rows.filter(
      (row) => recordingRoomIdentity(row.meeting_link) === room,
    );
    const occurrence =
      matching.length === 1 && calendarOccurrence(matching[0]!);
    if (occurrence) return occurrence;
    return {
      key: `room:${room}`,
      room,
      kind: matching.length > 1 ? "ambiguous" : "room",
    };
  }
  if (input.sessionId && !input.meeting)
    return { key: `session:${input.sessionId}`, room: null, kind: "session" };
  return { key: "unidentified", room: null, kind: "ambiguous" };
}

export function isRecordingExcluded(
  userId: string,
  occurrence: RecordingOccurrence,
) {
  const policy = account(userId);
  if (policy.excluded[occurrence.key]) return true;
  const exclusions = Object.values(policy.excluded);
  if (
    exclusions.some(
      (excluded) => excluded.kind === "ambiguous" && !excluded.room,
    )
  )
    return true;
  if (occurrence.kind === "calendar") {
    return exclusions.some(
      (excluded) =>
        excluded.kind !== "calendar" &&
        !!occurrence.room &&
        excluded.room === occurrence.room,
    );
  }
  return Object.values(policy.excluded).some((excluded) =>
    occurrence.room
      ? excluded.room === occurrence.room
      : occurrence.kind === "ambiguous",
  );
}

export function excludeRecordingOccurrence(
  userId: string,
  occurrence: RecordingOccurrence,
  sessionIds: string[] = [],
) {
  const policy = account(userId);
  policy.excluded[occurrence.key] = occurrence;
  for (const sessionId of sessionIds) {
    policy.sessions[sessionId] = true;
    policy.bindings[sessionId] = occurrence;
  }
  for (const [sessionId, binding] of Object.entries(policy.bindings)) {
    if (binding.key === occurrence.key) policy.sessions[sessionId] = true;
  }
  persist();
}

export function resumeRecordingOccurrence(
  userId: string,
  occurrence: RecordingOccurrence,
  newSessionId?: string,
) {
  const current = account(userId);
  if (
    newSessionId &&
    (current.sessions[newSessionId] || current.bindings[newSessionId])
  ) {
    throw new Error("Resume recording requires a new session");
  }
  const next = {
    ...current,
    excluded: { ...current.excluded },
    bindings: { ...current.bindings },
  };
  delete next.excluded[occurrence.key];
  for (const [key, excluded] of Object.entries(next.excluded)) {
    const unidentifiedHold = excluded.kind === "ambiguous" && !excluded.room;
    const roomHold =
      excluded.kind !== "calendar" &&
      !!occurrence.room &&
      excluded.room === occurrence.room;
    if (unidentifiedHold || roomHold) delete next.excluded[key];
  }
  if (newSessionId) next.bindings[newSessionId] = occurrence;
  const nextPolicy = { ...read(), [userId]: next };
  const value = JSON.stringify(nextPolicy);
  // Commit the relaxed policy only after persistence succeeds. A failed resume
  // must leave both the old hold and the old session tombstones effective.
  localStorage.setItem(STORAGE_KEY, value);
  memory = nextPolicy;
  savedValue = value;
  revision++;
  for (const listener of listeners) listener();
}

export function bindRecordingSession(
  userId: string,
  sessionId: string,
  occurrence: RecordingOccurrence,
) {
  const policy = account(userId);
  policy.bindings[sessionId] = occurrence;
  if (isRecordingExcluded(userId, occurrence))
    policy.sessions[sessionId] = true;
  persist();
}

export function isRecordingSessionExcluded(userId: string, sessionId: string) {
  const policy = account(userId);
  return (
    !!policy.sessions[sessionId] ||
    (!!policy.bindings[sessionId] &&
      isRecordingExcluded(userId, policy.bindings[sessionId]!))
  );
}

export function getRecordingOccurrenceSessionIds(
  userId: string,
  occurrence: RecordingOccurrence,
) {
  return Object.entries(account(userId).bindings)
    .filter(([, binding]) => binding.key === occurrence.key)
    .map(([sessionId]) => sessionId);
}
