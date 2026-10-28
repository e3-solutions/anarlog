import { useQuery } from "@tanstack/react-query";
import { useRef } from "react";

import { commands as detectCommands } from "@anlg/plugin-detect";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import { useSharingConfiguration } from "./configuration";
import { SharedRecordingEnrollment } from "./enrollment";
import {
  flushSharedRecordings,
  observeSharedRecording,
  sharedRecordingQueue,
  excludeSharedRecording,
} from "./native";

import { useAuth } from "~/auth";
import { env } from "~/env";
import { listenerStore } from "~/store/zustand/listener/instance";
import { selectAutomaticMeeting } from "~/stt/automatic-meeting";
import { replayNativeRecordingExclusions } from "~/stt/meeting-recording-control";
import { isRecordingSessionExcluded } from "~/stt/recording-exclusions";

export function SharedRecordingLifecycle() {
  const { session } = useAuth();
  if (!session || session.user.is_anonymous === true) return null;
  return (
    <>
      <SharedRecordingEnrollment
        key={`enrollment:${session.user.id}`}
        userId={session.user.id}
      />
      <ActiveSharedRecordingLifecycle
        key={session.user.id}
        userId={session.user.id}
      />
    </>
  );
}

function ActiveSharedRecordingLifecycle({ userId }: { userId: string }) {
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  const config = useSharingConfiguration(userId);
  const verified = useRef<{
    sessionId: string;
    scope: string;
    pid: number;
    room: string;
    occurrenceId: string;
  } | null>(null);
  useMountEffect(() => () => {
    verified.current = null;
    const live = listenerStore.getState().live;
    if (live.status === "active" && live.sessionId) {
      void observeSharedRecording({
        sessionId: live.sessionId,
        platform: "unverified",
        callIdentity: null,
        observedAtMs: Date.now(),
        accessToken: "",
        userId,
        quarantineReason: "observer_stopped",
      }).catch(() => {});
    }
  });
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  useQuery({
    queryKey: ["shared-recording-observation", userId],
    queryFn: async () => {
      const live = listenerStore.getState().live;
      const sessionId = live.sessionId;
      const inspection = await detectCommands
        .inspectMeetingAccessibility()
        .catch(() => null);
      if (live.status !== "active" || !sessionId) return { blocked: false };
      const inspected = inspection?.status === "ok" ? inspection.data : [];
      const active = inspected.filter((row) => row.activeCall);
      const first = active[0];
      const meetUrl = (identity: string | null | undefined) =>
        identity?.match(
          /^(https:\/\/meet\.google\.com\/[a-z]{3}-[a-z]{4}-[a-z]{3}):(?:window|tab):[a-zA-Z0-9]+$/,
        )?.[1];
      const url = first && meetUrl(first.callIdentity);
      // Multiple account windows may observe one conference, but never merge different calls.
      const sameConference =
        url &&
        active.every(
          (row) =>
            row.platform === "googleMeet" &&
            row.app.id === first.app.id &&
            row.pid === first.pid &&
            meetUrl(row.callIdentity) === url,
        );
      const rows = sameConference
        ? inspected.filter((row) => !row.activeCall || row === first)
        : inspected;
      const selected = selectAutomaticMeeting(rows, {
        googleMeet: true,
        slack: true,
      });
      const quarantine = async (
        quarantineReason: NonNullable<
          Parameters<typeof observeSharedRecording>[0]["quarantineReason"]
        >,
      ) => {
        verified.current = null;
        const current = listenerStore.getState().live;
        if (current.status !== "active" || current.sessionId !== sessionId)
          return { blocked: false };
        await observeSharedRecording({
          sessionId,
          platform: "unverified",
          callIdentity: null,
          observedAtMs: Date.now(),
          accessToken: "",
          userId,
          quarantineReason,
          diagnosticSummary: {
            inspectionStatus: inspection?.status ?? "unavailable",
            rowCount: Math.min(inspected.length, 4096),
            activeCallCount: Math.min(active.length, 4096),
            trustedCount: Math.min(
              inspected.filter((row) => row.accessibilityTrusted).length,
              4096,
            ),
            warningCategories: [
              ...new Set(
                inspected.flatMap((row) =>
                  row.warnings.flatMap((warning) =>
                    (
                      [
                        ["ambiguous", /ambiguous/i],
                        ["incomplete", /incomplete|truncat/i],
                        ["timeout", /timed?\s*out/i],
                        ["permission", /permission|trusted/i],
                        ["failed", /failed/i],
                        ["unscoped", /scop/i],
                        ["missing", /missing|did not expose|no uniquely/i],
                      ] as const
                    )
                      .filter(([, pattern]) => pattern.test(warning))
                      .map(([category]) => category),
                  ),
                ),
              ),
            ],
          },
        });
        return { blocked: true, quarantined: true };
      };
      const scope = JSON.stringify([
        userId,
        config.data?.organizationId,
        config.data?.apiBaseUrl,
        config.data?.connections
          .map((connection) => [
            connection.integrationId,
            connection.connectionId,
            connection.status,
          ])
          .sort(),
      ]);
      const known = verified.current;
      if (
        known?.sessionId === sessionId &&
        selected &&
        (selected.platform !== "googleMeet" ||
          selected.app.id !== "com.google.Chrome" ||
          selected.pid !== known.pid ||
          url !== known.room)
      )
        return quarantine("call_changed");
      let backgroundRoom: string | null = null;
      if (!selected) {
        if (inspection?.status !== "ok") return quarantine("ax_unavailable");
        if (
          active.length > 0 ||
          inspected.some((row) =>
            row.warnings.some((warning) => /ambiguous/i.test(warning)),
          )
        )
          return quarantine("ax_ambiguous");
        if (
          inspected.some(
            (row) =>
              row.platform === "googleMeet" &&
              !row.activeCall &&
              row.callIdentity,
          )
        )
          return quarantine("meeting_departed");
        if (!known || known.sessionId !== sessionId || known.scope !== scope)
          return quarantine("meeting_unverified");
        const chrome = inspected.filter(
          (row) => row.app.id === "com.google.Chrome",
        );
        if (
          !chrome.length ||
          chrome.some(
            (row) =>
              row.pid !== known.pid ||
              !row.accessibilityTrusted ||
              row.warnings.some((warning) =>
                /incomplete|truncat|timed?\s*out|permission|failed/i.test(
                  warning,
                ),
              ),
          )
        )
          return quarantine("ax_unavailable");
        const inventory = chrome[0]?.openCallIdentities;
        if (
          !Array.isArray(inventory) ||
          chrome.some(
            (row) =>
              JSON.stringify(row.openCallIdentities) !==
              JSON.stringify(inventory),
          )
        )
          return quarantine("inventory_missing");
        if (
          !inventory.length ||
          inventory.some(
            (identity) =>
              !/^https:\/\/meet\.google\.com\/[a-z]{3}-[a-z]{4}-[a-z]{3}:tab:[1-9]\d*$/.test(
                identity,
              ) || meetUrl(identity) !== known.room,
          )
        )
          return quarantine("inventory_conflict");
        backgroundRoom = known.room;
      }
      const session = await getSessionForRequest().catch(() => null);
      if (!session || session.user.id !== userId)
        return quarantine("auth_unavailable");
      const current = listenerStore.getState().live;
      if (current.status !== "active" || current.sessionId !== sessionId)
        return { blocked: false };
      const occurrenceId = await observeSharedRecording({
        sessionId,
        platform: selected?.platform ?? "googleMeet",
        callIdentity:
          backgroundRoom ??
          (selected?.platform === "googleMeet"
            ? (url ?? selected.callIdentity)
            : (selected?.callIdentity ?? null)),
        observedAtMs: Date.now(),
        accessToken: session.access_token,
        userId,
      });
      if (backgroundRoom && occurrenceId !== known?.occurrenceId)
        return quarantine("inventory_conflict");
      if (
        selected?.platform === "googleMeet" &&
        selected.app.id === "com.google.Chrome" &&
        url &&
        typeof occurrenceId === "string" &&
        occurrenceId.length > 0 &&
        listenerStore.getState().live.status === "active" &&
        listenerStore.getState().live.sessionId === sessionId
      ) {
        verified.current = {
          sessionId,
          scope,
          pid: selected.pid,
          room: url,
          occurrenceId,
        };
      }
      return { blocked: false };
    },
    enabled:
      config.data?.enabled === true &&
      config.data.apiBaseUrl === env.VITE_API_URL,
    refetchIntervalInBackground: true,
    refetchInterval: 4_000,
    retry: false,
  });
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  useQuery({
    queryKey: ["shared-recording-pump", userId, config.data?.organizationId],
    queryFn: async () => {
      const session = await getSessionForRequest();
      if (!session || session.user.id !== userId) return null;
      // Replay durable intent before the pump can retry pre-crash audio.
      await replayNativeRecordingExclusions(userId);
      const pending = await sharedRecordingQueue(userId);
      for (const job of pending) {
        if (isRecordingSessionExcluded(userId, job.sessionId))
          await excludeSharedRecording(job.sessionId, userId);
      }
      await flushSharedRecordings(session.access_token, userId);
      return Date.now();
    },
    enabled:
      config.data?.enabled === true &&
      config.data.apiBaseUrl === env.VITE_API_URL,
    refetchIntervalInBackground: true,
    refetchInterval: 10_000,
    retry: false,
  });
  return null;
}
