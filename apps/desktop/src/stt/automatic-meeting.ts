import type { MeetingAccessibilityInspection } from "@anlg/plugin-detect";

import {
  inspectionHasUncertainMeetingEvidence,
  inspectionShowsActiveMeeting,
} from "./meeting-accessibility";

export const AUTOMATIC_MEETING_POLL_MS = 2_000;
export const AUTOMATIC_MEETING_STOP_GRACE_MS = 15_000;
export const AUTOMATIC_MEETING_UNKNOWN_GRACE_MS = 60_000;

export type AutomaticMeetingOptions = {
  googleMeet: boolean;
  slack: boolean;
  teams?: boolean;
};

export type AutomaticMeeting = {
  key: string;
  app: { id: string; name: string };
  pid: number;
  callIdentity: string | null;
  platform: "googleMeet" | "slack" | "microsoftTeams";
  title: string;
};

function inspectionCallIdentity(inspection: MeetingAccessibilityInspection) {
  const value = "callIdentity" in inspection ? inspection.callIdentity : null;
  return typeof value === "string" && value.trim() ? value : null;
}

function browserIdentity(value: string | null) {
  const match = value?.match(/^(.*):(tab|window):([a-zA-Z0-9]+)$/);
  if (!match || (match[2] === "tab" && !/^[1-9]\d*$/.test(match[3]!)))
    return null;
  try {
    const url = new URL(match[1]!);
    if (
      url.protocol !== "https:" ||
      url.username ||
      url.password ||
      url.port ||
      ![
        "meet.google.com",
        "teams.microsoft.com",
        "teams.live.com",
        "teams.cloud.microsoft",
      ].includes(url.hostname)
    )
      return null;
    if (
      url.hostname === "meet.google.com" &&
      !/^\/[a-z]{3}-[a-z]{4}-[a-z]{3}$/.test(url.pathname)
    )
      return null;
    return {
      url: url.href,
      mode: match[2]!,
      id: match[3]!,
      platform:
        url.hostname === "meet.google.com" ? "googleMeet" : "microsoftTeams",
    };
  } catch {
    return null;
  }
}

/** A permission-driven identity upgrade must preserve capture and manual Stop. */
export function sameAutomaticMeeting(a: AutomaticMeeting, b: AutomaticMeeting) {
  if (a.key === b.key) return true;
  if (
    a.platform === "slack" ||
    b.platform !== a.platform ||
    a.app.id !== b.app.id ||
    a.pid !== b.pid
  )
    return false;
  const left = browserIdentity(a.callIdentity);
  const right = browserIdentity(b.callIdentity);
  return (
    a.callIdentity === b.callIdentity ||
    (a.platform === "microsoftTeams" &&
      left?.mode === "tab" &&
      right?.mode === "tab" &&
      left.id === right.id) ||
    (!!left && !!right && left.url === right.url && left.mode !== right.mode)
  );
}

export function sameSuppressedAutomaticMeeting(
  a: AutomaticMeeting,
  b: AutomaticMeeting,
) {
  if (sameAutomaticMeeting(a, b)) return true;
  if (
    a.platform !== "microsoftTeams" ||
    b.platform !== a.platform ||
    a.app.id !== b.app.id ||
    a.pid !== b.pid
  )
    return false;
  const left = browserIdentity(a.callIdentity);
  const right = browserIdentity(b.callIdentity);
  // Without tab inventory, URL changes cannot safely undo manual Stop.
  // This suppresses ambiguous tabs in that window, never ownership adoption.
  return (
    left?.mode === "window" && right?.mode === "window" && left.id === right.id
  );
}

function inventoryProvesDeparture(
  meeting: Pick<AutomaticMeeting, "app" | "pid" | "callIdentity" | "platform">,
  inspections: MeetingAccessibilityInspection[],
) {
  if (meeting.platform === "slack" || meeting.app.id !== "com.google.Chrome")
    return false;
  const identity = browserIdentity(meeting.callIdentity);
  if (!identity) return false;
  const inventories = inspections
    .filter((row) => row.app.id === meeting.app.id && row.pid === meeting.pid)
    .map((row) =>
      "openCallIdentities" in row ? row.openCallIdentities : undefined,
    );
  // Only a complete, valid, consistent inventory can establish absence.
  if (
    !inventories.length ||
    inventories.some(
      (value) =>
        !Array.isArray(value) ||
        value.some(
          (item) =>
            typeof item !== "string" || browserIdentity(item)?.mode !== "tab",
        ),
    )
  )
    return false;
  const first = inventories[0] as string[];
  if (
    inventories.some((value) => JSON.stringify(value) !== JSON.stringify(first))
  )
    return false;
  if (meeting.platform === "microsoftTeams") {
    // Teams can change its SPA route during the same call. Only closure of
    // the bound tab proves departure; a window identity cannot establish that.
    return (
      identity.mode === "tab" &&
      !first.some((value) => {
        const current = browserIdentity(value);
        return (
          current?.platform === "microsoftTeams" && current.id === identity.id
        );
      })
    );
  }
  return identity.mode === "tab"
    ? !first.includes(meeting.callIdentity!)
    : !first.some((value) => browserIdentity(value)?.url === identity.url);
}

function hasAmbiguousInspection(inspections: MeetingAccessibilityInspection[]) {
  return inspections.some((inspection) =>
    inspection.warnings.some((warning) => {
      if (/ambiguous/i.test(warning)) return true;
      // Older native builds report competing Huddles without saying ambiguous.
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
      (inspection.platform === "slack" && options.slack) ||
      (inspection.platform === "microsoftTeams" && options.teams)
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
    title:
      platform === "slack"
        ? "Slack huddle"
        : platform === "microsoftTeams"
          ? "Microsoft Teams"
          : "Google Meet",
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
    matching.some(inspectionHasUncertainMeetingEvidence)
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
