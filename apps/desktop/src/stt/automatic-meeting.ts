import type { MeetingAccessibilityInspection } from "@anlg/plugin-detect";

import { inspectionShowsActiveMeeting } from "./meeting-accessibility";

export const AUTOMATIC_MEETING_POLL_MS = 2_000;
export const AUTOMATIC_MEETING_STOP_GRACE_MS = 15_000;
export const AUTOMATIC_MEETING_UNKNOWN_GRACE_MS = 60_000;

export type AutomaticMeetingOptions = {
  googleMeet: boolean;
  slack: boolean;
};

export type AutomaticMeeting = {
  key: string;
  app: { id: string; name: string };
  pid: number;
  callIdentity: string | null;
  platform: "googleMeet" | "slack";
  title: string;
};

function inspectionCallIdentity(inspection: MeetingAccessibilityInspection) {
  const value = inspection.callIdentity;
  return typeof value === "string" && value.trim() ? value : null;
}

function meetIdentity(value: string | null) {
  const match = value?.match(
    /^(https:\/\/meet\.google\.com\/[a-z]{3}-[a-z]{4}-[a-z]{3}):(tab|window):([a-zA-Z0-9]+)$/,
  );
  if (!match || (match[2] === "tab" && !/^[1-9]\d*$/.test(match[3]!)))
    return null;
  return { url: match[1]!, mode: match[2]! };
}

/** A permission-driven identity upgrade must preserve capture and manual Stop. */
export function sameAutomaticMeeting(a: AutomaticMeeting, b: AutomaticMeeting) {
  if (a.key === b.key) return true;
  if (
    a.platform !== "googleMeet" ||
    b.platform !== a.platform ||
    a.app.id !== b.app.id ||
    a.pid !== b.pid
  )
    return false;
  const left = meetIdentity(a.callIdentity);
  const right = meetIdentity(b.callIdentity);
  return (
    a.callIdentity === b.callIdentity ||
    (!!left && !!right && left.url === right.url && left.mode !== right.mode)
  );
}

function inventoryProvesDeparture(
  meeting: Pick<AutomaticMeeting, "app" | "pid" | "callIdentity" | "platform">,
  inspections: MeetingAccessibilityInspection[],
) {
  if (
    meeting.platform !== "googleMeet" ||
    meeting.app.id !== "com.google.Chrome"
  )
    return false;
  const identity = meetIdentity(meeting.callIdentity);
  if (!identity) return false;
  const inventories = inspections
    .filter((row) => row.app.id === meeting.app.id && row.pid === meeting.pid)
    .map((row) => row.openCallIdentities);
  // Only a complete, valid, consistent inventory can establish absence.
  if (
    !inventories.length ||
    inventories.some(
      (value) =>
        !Array.isArray(value) ||
        value.some(
          (item) =>
            typeof item !== "string" || meetIdentity(item)?.mode !== "tab",
        ),
    )
  )
    return false;
  const first = inventories[0] as string[];
  if (
    inventories.some((value) => JSON.stringify(value) !== JSON.stringify(first))
  )
    return false;
  return identity.mode === "tab"
    ? !first.includes(meeting.callIdentity!)
    : !first.some((value) => meetIdentity(value)?.url === identity.url);
}

function hasAmbiguousInspection(inspections: MeetingAccessibilityInspection[]) {
  return inspections.some((inspection) =>
    inspection.warnings.some((warning) => {
      if (/ambiguous/i.test(warning)) return true;
      // Slack reports competing Huddles without using the word "ambiguous".
      const huddles = warning.match(
        /^Slack exposed (\d+) active Huddle windows; inspection is scoped to none$/i,
      );
      return inspection.platform === "slack" && Number(huddles?.[1]) > 1;
    }),
  );
}

/** Positive local call evidence, independent of calendar and microphone mute. */
export function selectAutomaticMeeting(
  inspections: MeetingAccessibilityInspection[],
  options: AutomaticMeetingOptions,
): AutomaticMeeting | null {
  if (hasAmbiguousInspection(inspections)) return null;
  const candidates = inspections.filter((inspection) => inspection.activeCall);
  // Multiple real calls are ambiguous too: never silently select the first.
  if (candidates.length !== 1) return null;
  const inspection = candidates[0]!;
  if (
    !inspectionShowsActiveMeeting(inspection) ||
    !(
      (inspection.platform === "googleMeet" && options.googleMeet) ||
      (inspection.platform === "slack" && options.slack)
    )
  )
    return null;
  const platform = inspection.platform as AutomaticMeeting["platform"];
  const callIdentity = inspectionCallIdentity(inspection);
  const meeting: AutomaticMeeting = {
    key: `${platform}:${inspection.app.id}:${inspection.pid}${callIdentity ? `:${callIdentity}` : ""}`,
    app: inspection.app,
    pid: inspection.pid,
    callIdentity,
    platform,
    title: platform === "slack" ? "Slack huddle" : "Google Meet",
  };
  return inventoryProvesDeparture(meeting, inspections) ? null : meeting;
}

/** Missing or unreadable evidence is not a confirmed end of a muted call. */
export function automaticMeetingObservation(
  meeting: AutomaticMeeting,
  inspections: MeetingAccessibilityInspection[],
): "active" | "inactive" | "unknown" {
  if (inventoryProvesDeparture(meeting, inspections)) return "inactive";
  if (hasAmbiguousInspection(inspections)) return "unknown";
  const matching = inspections.filter(
    (inspection) =>
      inspection.app.id === meeting.app.id &&
      inspection.pid === meeting.pid &&
      (!meeting.callIdentity ||
        (inspection.platform === meeting.platform &&
          (inspectionCallIdentity(inspection) === meeting.callIdentity ||
            inspectionShowsActiveMeeting(inspection)) &&
          sameAutomaticMeeting(meeting, {
            ...meeting,
            key: `${inspection.app.id}:${inspection.pid}:${inspectionCallIdentity(inspection)}`,
            callIdentity: inspectionCallIdentity(inspection),
          }))),
  );
  if (
    matching.some(
      (inspection) =>
        inspection.platform === meeting.platform &&
        inspectionShowsActiveMeeting(inspection),
    )
  ) {
    return "active";
  }
  if (
    matching.length === 0 ||
    matching.some(
      (inspection) =>
        !inspection.accessibilityTrusted ||
        inspection.warnings.some((warning) =>
          /ambiguous|incomplete|no uniquely scoped|truncat|timed?\s*out|permission|failed/i.test(
            warning,
          ),
        ),
    )
  ) {
    return "unknown";
  }
  return "inactive";
}

export function automaticMeetingIsActive(
  meeting: AutomaticMeeting,
  inspections: MeetingAccessibilityInspection[],
) {
  return automaticMeetingObservation(meeting, inspections) === "active";
}
