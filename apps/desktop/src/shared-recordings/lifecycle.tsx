import { Trans } from "@lingui/react/macro";
import {
  skipToken,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useRef } from "react";

import { commands as detectCommands } from "@anlg/plugin-detect";
import { Button } from "@anlg/ui/components/ui/button";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import {
  sharedRecordingsPreviewUrl,
  type SharedRecordingAccount,
} from "./client";
import {
  configureSharedRecordings,
  flushSharedRecordings,
  observeSharedRecording,
  sharedRecordingConfiguration,
  sharedRecordingQueue,
} from "./native";

import { useAuth } from "~/auth";
import { useOpenIntegrationUrl } from "~/shared/integration";
import { listenerStore } from "~/store/zustand/listener/instance";
import { selectAutomaticMeeting } from "~/stt/automatic-meeting";

function useSharingConfiguration(userId: string) {
  return useQuery({
    queryKey: ["shared-recording-configuration", userId],
    queryFn: async () => {
      const config = await sharedRecordingConfiguration();
      return config?.userId === userId ? config : null;
    },
    refetchIntervalInBackground: true,
    refetchInterval: 5_000,
    retry: false,
  });
}

export function SharedRecordingLifecycle() {
  const { session } = useAuth();
  if (
    !sharedRecordingsPreviewUrl ||
    !session ||
    session.user.is_anonymous === true
  )
    return null;
  return (
    <ActiveSharedRecordingLifecycle
      key={session.user.id}
      userId={session.user.id}
    />
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
      config.data.apiBaseUrl === sharedRecordingsPreviewUrl,
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
      await flushSharedRecordings(session.access_token, userId);
      return Date.now();
    },
    enabled:
      config.data?.enabled === true &&
      config.data.apiBaseUrl === sharedRecordingsPreviewUrl,
    refetchIntervalInBackground: true,
    refetchInterval: 10_000,
    retry: false,
  });
  return null;
}

export function SharedRecordingPreferences({
  account,
  organizationId,
}: {
  account: SharedRecordingAccount;
  organizationId: string;
}) {
  const config = useSharingConfiguration(account.userId);
  const queryClient = useQueryClient();
  const { openIntegration, openingAction } = useOpenIntegrationUrl();
  const enabled =
    config.data?.enabled === true &&
    config.data.organizationId === organizationId;
  const update = useMutation({
    mutationFn: () =>
      configureSharedRecordings({
        enabled: !enabled,
        apiBaseUrl: sharedRecordingsPreviewUrl!,
        organizationId,
        userId: account.userId,
        connections: account.connections,
      }),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: ["shared-recording-configuration", account.userId],
      }),
  });
  return (
    <div className="flex items-start justify-between gap-4 rounded-lg border p-3">
      <div className="space-y-1 text-sm">
        <p className="font-medium">
          {enabled ? (
            <Trans>Automatic sharing is on for this organization</Trans>
          ) : (
            <Trans>Automatic sharing is off</Trans>
          )}
        </p>
        <p className="text-muted-foreground">
          <Trans>
            Only verified Google Meet and Slack huddle recordings are shared.
            Original audio stays local when offline; pending uploads retry after
            reconnecting.
          </Trans>
        </p>
        <p className="text-muted-foreground">
          <Trans>
            Sharing keeps a private local copy for retries, including when local
            audio retention is otherwise off. Organization members can read the
            published shared transcript.
          </Trans>
        </p>
        {!account.connections.some(
          (connection) => connection.status === "connected",
        ) ? (
          <p className="text-muted-foreground">
            <Trans>Connect Google Meet or Slack before enabling sharing.</Trans>
          </p>
        ) : null}
        <div className="flex gap-2">
          {(["google-meet", "slack"] as const).map((integrationId) => {
            const connection = account.connections.find(
              (connection) => connection.integrationId === integrationId,
            );
            if (connection?.status === "connected") return null;
            return (
              <Button
                key={integrationId}
                variant="outline"
                disabled={openingAction !== null || enabled}
                onClick={() =>
                  openIntegration({
                    nangoIntegrationId: integrationId,
                    connectionId: connection?.connectionId,
                    action: connection ? "reconnect" : "connect",
                  })
                }
              >
                {integrationId === "google-meet" ? (
                  <Trans>Connect Google Meet</Trans>
                ) : (
                  <Trans>Connect Slack</Trans>
                )}
              </Button>
            );
          })}
        </div>
        {enabled &&
        ["google-meet", "slack"].some(
          (integrationId) =>
            !account.connections.some(
              (connection) =>
                connection.integrationId === integrationId &&
                connection.status === "connected",
            ),
        ) ? (
          <p className="text-muted-foreground">
            <Trans>Turn off sharing before connecting another provider.</Trans>
          </p>
        ) : null}
        {config.error || update.error ? (
          <p role="alert">
            <Trans>
              Sharing settings could not be saved. Local recording is
              unaffected.
            </Trans>
          </p>
        ) : null}
      </div>
      <Button
        variant={enabled ? "outline" : "default"}
        disabled={
          config.isPending ||
          update.isPending ||
          (!enabled &&
            !account.connections.some(
              (connection) => connection.status === "connected",
            ))
        }
        onClick={() => update.mutate()}
      >
        {enabled ? <Trans>Turn off</Trans> : <Trans>Enable sharing</Trans>}
      </Button>
    </div>
  );
}

export function SharedRecordingQueue({ userId }: { userId: string }) {
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  const observation = useQuery<{ blocked: boolean; quarantined?: boolean }>({
    queryKey: ["shared-recording-observation", userId],
    queryFn: skipToken,
    enabled: false,
  });
  const queue = useQuery({
    queryKey: ["shared-recording-queue", userId],
    queryFn: () => sharedRecordingQueue(userId),
    refetchIntervalInBackground: true,
    refetchInterval: 5_000,
    retry: false,
  });
  const retry = useMutation({
    mutationFn: async () => {
      const session = await getSessionForRequest();
      if (!session || session.user.id !== userId)
        throw new Error("Account changed.");
      await flushSharedRecordings(session.access_token, userId);
    },
    onSettled: () => queue.refetch(),
  });
  const pending = queue.data?.filter((job) => job.state !== "uploaded") ?? [];
  const quarantined = pending.filter((job) => job.state.startsWith("blocked_"));
  const retryable = pending.filter((job) => !job.state.startsWith("blocked_"));
  const reasons = [
    ...new Set(quarantined.map((job) => job.error).filter(Boolean)),
  ];
  if (queue.error)
    return (
      <p role="alert" className="text-sm">
        <Trans>
          The upload queue is unavailable. Local recording is unaffected.
        </Trans>
      </p>
    );
  if (!pending.length)
    return observation.error || observation.data?.blocked ? (
      <p role="status" className="text-muted-foreground text-sm">
        {observation.data?.quarantined ? (
          "Meeting activity could not be verified throughout this recording. Original audio stays local; start a new recording in a new note with one verified call to share."
        ) : (
          <Trans>
            Meeting identity could not be verified. Recording stays local;
            sharing will retry while the call is active.
          </Trans>
        )}
      </p>
    ) : null;
  return (
    <div
      className="flex items-center justify-between gap-3 text-sm"
      role="status"
    >
      <span>
        {retryable.length > 0 ? (
          <>
            {retryable.length} <Trans>recordings waiting to share</Trans>
          </>
        ) : null}
        {retryable.some((job) => job.state === "error")
          ? " · upload will retry"
          : ""}
        {quarantined.length > 0 ? (
          <span className="block" role="alert">
            {quarantined.length}{" "}
            {quarantined.length === 1
              ? "recording kept locally."
              : "recordings kept locally."}
            {reasons.map((reason) => (
              <span key={reason} className="block">
                {reason}
              </span>
            ))}
          </span>
        ) : null}
      </span>
      {retryable.length > 0 ? (
        <Button
          variant="outline"
          size="sm"
          disabled={retry.isPending}
          onClick={() => retry.mutate()}
        >
          <Trans>Retry uploads</Trans>
        </Button>
      ) : null}
    </div>
  );
}
