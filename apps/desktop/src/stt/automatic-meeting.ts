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
  platform: "googleMeet" | "slack";
  title: string;
};

/** Positive local call evidence, independent of calendar and microphone mute. */
export function selectAutomaticMeeting(
  inspections: MeetingAccessibilityInspection[],
  options: AutomaticMeetingOptions,
): AutomaticMeeting | null {
  const candidates = inspections.filter(
    (inspection) =>
      ((inspection.platform === "googleMeet" && options.googleMeet) ||
        (inspection.platform === "slack" && options.slack)) &&
      inspectionShowsActiveMeeting(inspection),
  );
  // Multiple real calls are ambiguous too: never silently select the first.
  if (candidates.length !== 1) return null;
  const inspection = candidates[0]!;
  const platform = inspection.platform as AutomaticMeeting["platform"];
  return {
    key: `${platform}:${inspection.app.id}:${inspection.pid}`,
    app: inspection.app,
    pid: inspection.pid,
    platform,
    title: platform === "slack" ? "Slack huddle" : "Google Meet",
  };
}

/** Missing or unreadable evidence is not a confirmed end of a muted call. */
export function automaticMeetingObservation(
  meeting: AutomaticMeeting,
  inspections: MeetingAccessibilityInspection[],
): "active" | "inactive" | "unknown" {
  const matching = inspections.filter(
    (inspection) =>
      inspection.app.id === meeting.app.id && inspection.pid === meeting.pid,
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
