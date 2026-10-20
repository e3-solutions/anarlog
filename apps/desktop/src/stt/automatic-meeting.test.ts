import { describe, expect, it } from "vitest";

import type { MeetingAccessibilityInspection } from "@anlg/plugin-detect";

import {
  automaticMeetingObservation,
  selectAutomaticMeeting,
  sameAutomaticMeeting,
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
  ...{ callIdentity: null },
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

  it("tracks distinct call contexts in one browser without confusing home or another departure", () => {
    const joined = { ...meet, callIdentity: "window-a:meet-a" };
    const home = { ...meet, platform: "unknown" as const, activeCall: false };
    const meeting = selectAutomaticMeeting([home, joined], options)!;
    expect(meeting.callIdentity).toBe(joined.callIdentity);
    const other = { ...meet, callIdentity: "window-b:meet-b" };
    expect(selectAutomaticMeeting([joined, other], options)).toBeNull();
    expect(selectAutomaticMeeting([other], options)?.key).not.toBe(meeting.key);
    for (const inspections of [
      [home],
      [{ ...other, activeCall: false }, home],
      [other, home],
    ]) {
      expect(automaticMeetingObservation(meeting, inspections)).toBe("unknown");
    }
    expect(
      automaticMeetingObservation(meeting, [
        { ...joined, activeCall: false },
        home,
      ]),
    ).toBe("inactive");
    expect(automaticMeetingObservation(meeting, [joined, home])).toBe("active");
  });

  it("honors global ambiguity and unsupported competing calls instead of selecting a supported row", () => {
    const meeting = selectAutomaticMeeting([meet], options)!;
    const ambiguous = {
      ...meet,
      activeCall: false,
      platform: "unknown" as const,
      warnings: ["browser contexts ambiguous"],
    };
    expect(selectAutomaticMeeting([meet, ambiguous], options)).toBeNull();
    expect(
      automaticMeetingObservation(meeting, [
        { ...meet, activeCall: false },
        ambiguous,
      ]),
    ).toBe("unknown");
    expect(
      selectAutomaticMeeting([meet, { ...meet, platform: "zoom" }], options),
    ).toBeNull();
  });

  it("treats legacy multiple-Huddle scope warnings as uncertainty for stopping and competing starts", () => {
    const slack = {
      ...meet,
      platform: "slack" as const,
      app: { id: "com.tinyspeck.slackmacgap", name: "Slack" },
    };
    const meeting = selectAutomaticMeeting([slack], options)!;
    const uncertain = {
      ...slack,
      activeCall: false,
      windowTitle: null,
      warnings: [
        "Slack exposed 2 active Huddle windows; inspection is scoped to none",
      ],
    };
    expect(automaticMeetingObservation(meeting, [uncertain])).toBe("unknown");
    expect(selectAutomaticMeeting([uncertain, meet], options)).toBeNull();
    expect(
      selectAutomaticMeeting(
        [
          {
            ...uncertain,
            warnings: [
              "Slack exposed 0 active Huddle windows; inspection is scoped to none",
            ],
          },
          meet,
        ],
        options,
      )?.platform,
    ).toBe("googleMeet");
  });

  it("ignores malformed identity fields while preserving legacy app and process identity", () => {
    const malformed = {
      ...meet,
      callIdentity: 42,
    } as unknown as MeetingAccessibilityInspection;
    expect(selectAutomaticMeeting([malformed], options)?.key).toBe(
      selectAutomaticMeeting([meet], options)?.key,
    );
  });
  it("uses complete tab inventory to distinguish quiet calls from closure and stale AX", () => {
    const identity = "https://meet.google.com/abc-defg-hij:tab:42";
    const joined = {
      ...meet,
      callIdentity: identity,
      ...{ openCallIdentities: [identity] },
    };
    const meeting = selectAutomaticMeeting([joined], options)!;
    const hidden = {
      ...joined,
      activeCall: false,
      platform: "unknown" as const,
      callIdentity: null,
      warnings: ["no uniquely scoped browser root"],
    };
    expect(automaticMeetingObservation(meeting, [hidden])).toBe("unknown");
    expect(
      automaticMeetingObservation(meeting, [
        { ...hidden, ...{ openCallIdentities: [] } },
      ]),
    ).toBe("inactive");
    expect(
      selectAutomaticMeeting(
        [{ ...joined, ...{ openCallIdentities: [] } }],
        options,
      ),
    ).toBeNull();
    for (const value of [undefined, null, ["invalid"], [42]]) {
      const unavailable = {
        ...hidden,
        ...{ openCallIdentities: value },
      } as unknown as MeetingAccessibilityInspection;
      expect(automaticMeetingObservation(meeting, [unavailable])).toBe(
        "unknown",
      );
    }
    const other = "https://meet.google.com/aaa-bbbb-ccc:tab:43";
    expect(
      automaticMeetingObservation(meeting, [
        { ...hidden, ...{ openCallIdentities: [identity, other] } },
      ]),
    ).toBe("unknown");
    expect(
      automaticMeetingObservation(meeting, [
        { ...hidden, ...{ openCallIdentities: [other] } },
      ]),
    ).toBe("inactive");
  });

  it("preserves ownership across legacy identity upgrades without aliasing distinct tabs", () => {
    const row = (identity: string) => ({ ...meet, callIdentity: identity });
    const legacy = selectAutomaticMeeting(
      [row("https://meet.google.com/abc-defg-hij:window:abc")],
      options,
    )!;
    const stable = selectAutomaticMeeting(
      [row("https://meet.google.com/abc-defg-hij:tab:42")],
      options,
    )!;
    const other = selectAutomaticMeeting(
      [row("https://meet.google.com/abc-defg-hij:tab:43")],
      options,
    )!;
    expect(sameAutomaticMeeting(legacy, stable)).toBe(true);
    expect(sameAutomaticMeeting(stable, other)).toBe(false);
    expect(
      automaticMeetingObservation(stable, [
        {
          ...row(legacy.callIdentity!),
          activeCall: false,
          ...{
            openCallIdentities: [stable.callIdentity!, other.callIdentity!],
          },
        },
      ]),
    ).toBe("unknown");
    expect(
      automaticMeetingObservation(legacy, [row(stable.callIdentity!)]),
    ).toBe("active");
    expect(
      automaticMeetingObservation(stable, [row(legacy.callIdentity!)]),
    ).toBe("active");
    expect(
      automaticMeetingObservation(legacy, [
        { ...meet, ...{ openCallIdentities: [] } },
      ]),
    ).toBe("inactive");
    expect(
      automaticMeetingObservation(legacy, [
        {
          ...meet,
          activeCall: false,
          callIdentity: null,
          platform: "unknown" as const,
          ...{
            openCallIdentities: [stable.callIdentity!, other.callIdentity!],
          },
        },
      ]),
    ).toBe("unknown");
  });
});
