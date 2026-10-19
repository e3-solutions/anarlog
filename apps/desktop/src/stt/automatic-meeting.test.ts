import { describe, expect, it } from "vitest";

import type { MeetingAccessibilityInspection } from "@anlg/plugin-detect";

import {
  automaticMeetingObservation,
  selectAutomaticMeeting,
} from "./automatic-meeting";

const options = { googleMeet: true, slack: true };
const meet: MeetingAccessibilityInspection = {
  activeCall: true,
  app: { id: "com.google.Chrome", name: "Chrome" },
  pid: 123,
  platform: "googleMeet",
  surface: "web",
  accessibilityTrusted: true,
  windowTitle: "Meet - abc-defg-hij",
  remoteScreenShare: false,
  warnings: [],
};

describe("automatic meeting evidence", () => {
  it("accepts joined ad hoc Meet without calendar or an open microphone", () => {
    expect(selectAutomaticMeeting([meet], options)).toMatchObject({
      platform: "googleMeet",
      app: meet.app,
    });
  });

  it("rejects browser microphone use without validated joined-call controls", () => {
    expect(
      selectAutomaticMeeting([{ ...meet, activeCall: false }], options),
    ).toBeNull();
    expect(
      selectAutomaticMeeting([{ ...meet, platform: "unknown" }], options),
    ).toBeNull();
  });

  it("uses the same supported-platform policy for Slack", () => {
    const slack = {
      ...meet,
      platform: "slack" as const,
      app: { id: "com.tinyspeck.slackmacgap", name: "Slack" },
    };
    expect(selectAutomaticMeeting([slack], options)?.platform).toBe("slack");
    expect(
      selectAutomaticMeeting([slack], { ...options, slack: false }),
    ).toBeNull();
    expect(
      selectAutomaticMeeting([meet], { ...options, googleMeet: false }),
    ).toBeNull();
  });

  it("rejects ambiguous calls and inaccessible or incomplete snapshots", () => {
    expect(
      selectAutomaticMeeting([meet, { ...meet, pid: 456 }], options),
    ).toBeNull();
    for (const inspection of [
      { ...meet, accessibilityTrusted: false },
      { ...meet, warnings: ["snapshot incomplete"] },
      { ...meet, warnings: ["ambiguous scope"] },
    ]) {
      expect(selectAutomaticMeeting([inspection], options)).toBeNull();
    }
  });

  it("keeps identity stable when the meeting window title changes", () => {
    expect(
      selectAutomaticMeeting(
        [{ ...meet, windowTitle: "Meet - new title" }],
        options,
      )?.key,
    ).toBe(selectAutomaticMeeting([meet], options)?.key);
  });

  it("does not treat unreadable or missing evidence as a confirmed end", () => {
    const meeting = selectAutomaticMeeting([meet], options)!;
    expect(automaticMeetingObservation(meeting, [meet])).toBe("active");
    expect(automaticMeetingObservation(meeting, [])).toBe("unknown");
    expect(automaticMeetingObservation(meeting, [{ ...meet, pid: 456 }])).toBe(
      "unknown",
    );
    expect(
      automaticMeetingObservation(meeting, [
        { ...meet, accessibilityTrusted: false },
      ]),
    ).toBe("unknown");
    expect(
      automaticMeetingObservation(meeting, [
        { ...meet, activeCall: false, warnings: ["snapshot incomplete"] },
      ]),
    ).toBe("unknown");
    expect(
      automaticMeetingObservation(meeting, [{ ...meet, activeCall: false }]),
    ).toBe("inactive");
  });
});
