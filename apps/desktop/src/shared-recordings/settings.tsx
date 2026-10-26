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
          {enabled
            ? "Automatic sharing is on for this organization"
            : "Automatic sharing is off"}
        </p>
        <p className="text-muted-foreground">
          Only verified Google Meet and Slack huddle recordings are shared.
          Original audio stays local when offline; pending uploads retry after
          reconnecting.
        </p>
        <p className="text-muted-foreground">
          Sharing keeps a private local copy for retries, including when local
          audio retention is otherwise off. Organization members can read the
          published shared transcript.
        </p>
        {!account.connections.some(
          (connection) => connection.status === "connected",
        ) ? (
          <p className="text-muted-foreground">
            Connect Google Meet or Slack before enabling sharing.
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
                {integrationId === "google-meet"
                  ? "Connect Google Meet"
                  : "Connect Slack"}
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
            Turn off sharing before connecting another provider.
          </p>
        ) : null}
        {config.error || update.error ? (
          <p role="alert">
            Sharing settings could not be saved. Local recording is unaffected.
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
        {enabled ? "Turn off" : "Enable sharing"}
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
        The upload queue is unavailable. Local recording is unaffected.
      </p>
    );
  if (!pending.length)
    return observation.error || observation.data?.blocked ? (
      <p role="status" className="text-muted-foreground text-sm">
        {observation.data?.quarantined
          ? "Meeting activity could not be verified throughout this recording. Original audio stays local; start a new recording in a new note with one verified call to share."
          : "Meeting identity could not be verified. Recording stays local; sharing will retry while the call is active."}
      </p>
    ) : null;
  return (
    <div
      className="flex items-center justify-between gap-3 text-sm"
      role="status"
    >
      <span>
        {retryable.length > 0 ? (
          <>{retryable.length} recordings waiting to share</>
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
          Retry uploads
        </Button>
      ) : null}
    </div>
  );
}
