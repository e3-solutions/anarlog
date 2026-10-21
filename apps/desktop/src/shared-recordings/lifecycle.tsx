import { Trans } from "@lingui/react/macro";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { commands as detectCommands } from "@anlg/plugin-detect";
import { Button } from "@anlg/ui/components/ui/button";

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
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  useQuery({
    queryKey: ["shared-recording-observation", userId],
    queryFn: async () => {
      const live = listenerStore.getState().live;
      if (live.status !== "active" || !live.sessionId)
        return { blocked: false };
      const inspection = await detectCommands.inspectMeetingAccessibility();
      if (inspection.status !== "ok") return { blocked: true };
      const active = inspection.data.filter((row) => row.activeCall);
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
        ? inspection.data.filter((row) => !row.activeCall || row === first)
        : inspection.data;
      const selected = selectAutomaticMeeting(rows, {
        googleMeet: true,
        slack: true,
      });
      if (!selected) return { blocked: true };
      const session = await getSessionForRequest();
      if (!session || session.user.id !== userId) return { blocked: true };
      if (listenerStore.getState().live.sessionId !== live.sessionId)
        return { blocked: false };
      await observeSharedRecording({
        sessionId: live.sessionId,
        platform: selected.platform,
        callIdentity: selected.callIdentity,
        observedAtMs: Date.now(),
        accessToken: session.access_token,
        userId,
      });
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
  const observation = useQuery<{ blocked: boolean }>({
    queryKey: ["shared-recording-observation", userId],
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
        <Trans>
          Meeting identity could not be verified. Recording stays local; sharing
          will retry while the call is active.
        </Trans>
      </p>
    ) : null;
  return (
    <div
      className="flex items-center justify-between gap-3 text-sm"
      role="status"
    >
      <span>
        {pending.length} <Trans>recordings waiting to share</Trans>
        {pending.some((job) => job.state === "error")
          ? " · upload will retry"
          : ""}
      </span>
      <Button
        variant="outline"
        size="sm"
        disabled={retry.isPending}
        onClick={() => retry.mutate()}
      >
        <Trans>Retry uploads</Trans>
      </Button>
    </div>
  );
}
