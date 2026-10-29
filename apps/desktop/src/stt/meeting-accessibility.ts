import type { MeetingAccessibilityInspection } from "@anlg/plugin-detect";

export function inspectionHasUncertainMeetingEvidence(
  inspection: MeetingAccessibilityInspection,
) {
  return (
    !inspection.accessibilityTrusted ||
    (inspection.activeCall &&
      (!inspection.windowTitle?.trim() || inspection.platform === "unknown")) ||
    inspection.warnings.some((warning) =>
      /ambiguous|incomplete|no uniquely|truncat|timed?\s*out|permission|failed/i.test(
        warning,
      ),
    )
  );
}

export function inspectionShowsActiveMeeting(
  inspection: MeetingAccessibilityInspection,
): boolean {
  return Boolean(
    inspection.activeCall &&
    inspection.platform !== "unknown" &&
    inspection.windowTitle?.trim() &&
    !inspectionHasUncertainMeetingEvidence(inspection),
  );
}

export function inspectionsShowActiveMeetingForApps(
  inspections: MeetingAccessibilityInspection[],
  appIds: string[],
) {
  const expectedAppIds = new Set(appIds);
  return inspections.some(
    (inspection) =>
      expectedAppIds.has(inspection.app.id) &&
      inspectionShowsActiveMeeting(inspection),
  );
}

/** Unknown accessibility must not turn microphone mute into a call departure. */
export function meetingObservationForApps(
  inspections: MeetingAccessibilityInspection[] | null,
  appIds: string[],
): "active" | "inactive" | "unknown" {
  if (!inspections) return "unknown";
  if (inspectionsShowActiveMeetingForApps(inspections, appIds)) return "active";
  const matching = inspections.filter((row) => appIds.includes(row.app.id));
  return matching.some(inspectionHasUncertainMeetingEvidence)
    ? "unknown"
    : "inactive";
}
