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
  const value = "callIdentity" in inspection ? inspection.callIdentity : null;
  return typeof value === "string" && value.trim() ? value : null;
}

function hasAmbiguousInspection(inspections: MeetingAccessibilityInspection[]) {
  return inspections.some((inspection) =>
    inspection.warnings.some((warning) => /ambiguous/i.test(warning)),
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
  return {
    key: `${platform}:${inspection.app.id}:${inspection.pid}${callIdentity ? `:${callIdentity}` : ""}`,
    app: inspection.app,
    pid: inspection.pid,
    callIdentity,
    platform,
    title: platform === "slack" ? "Slack huddle" : "Google Meet",
  };
}

/** Missing or unreadable evidence is not a confirmed end of a muted call. */
export function automaticMeetingObservation(
  meeting: AutomaticMeeting,
  inspections: MeetingAccessibilityInspection[],
): "active" | "inactive" | "unknown" {
  if (hasAmbiguousInspection(inspections)) return "unknown";
  const matching = inspections.filter(
    (inspection) =>
      inspection.app.id === meeting.app.id &&
      inspection.pid === meeting.pid &&
      (!meeting.callIdentity ||
        (inspectionCallIdentity(inspection) === meeting.callIdentity &&
          inspection.platform === meeting.platform)),
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
