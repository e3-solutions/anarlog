import { Trans } from "@lingui/react/macro";
import {
  skipToken,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";

import { Button } from "@anlg/ui/components/ui/button";

import {
  sharedRecordingsPreviewUrl,
  type SharedRecordingAccount,
} from "./client";
import { useSharingConfiguration } from "./configuration";
import {
  configureSharedRecordings,
  flushSharedRecordings,
  sharedRecordingQueue,
} from "./native";

import { useAuth } from "~/auth";
import { useOpenIntegrationUrl } from "~/shared/integration";

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
