import {
  skipToken,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";

import { Button } from "@anlg/ui/components/ui/button";

import type { SharedRecordingAccount } from "./client";
import { useSharingConfiguration } from "./configuration";
import {
  configureSharedRecordings,
  flushSharedRecordings,
  sharedRecordingQueue,
} from "./native";

import { useAuth } from "~/auth";
import { env } from "~/env";
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
    config.data.organizationId === organizationId &&
    config.data.apiBaseUrl === env.VITE_API_URL;
  const managed = account.policy?.workspaceId === organizationId;
  const connected = (integrationId: string) =>
    account.connections.some(
      (connection) =>
        connection.integrationId === integrationId &&
        connection.status === "connected",
    );
  const ready = managed
    ? ["google-meet", "google-calendar"].every(connected)
    : ["google-meet", "slack"].some(connected);
  const update = useMutation({
    mutationFn: () =>
      configureSharedRecordings({
        enabled: !enabled,
        apiBaseUrl: env.VITE_API_URL,
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
    <div className="flex flex-col gap-4">
      <div className="space-y-1 text-sm">
        <p className="font-medium">
          {enabled ? "Automatic sharing is on" : "Automatic sharing is off"}
        </p>
        <p className="text-muted-foreground">
          Original meeting audio and transcripts are saved to the organization’s
          database. Uploads resume when you reconnect.
        </p>
        <p className="text-muted-foreground">
          A local audio copy is kept for upload retries, even when audio
          retention is off.
        </p>
        {!ready ? (
          <p className="text-muted-foreground">
            {managed
              ? "Sign in with Google to connect Meet and your calendar."
              : "Connect Google Meet or Slack to turn on sharing."}
          </p>
        ) : null}
        <div className="flex flex-wrap gap-2">
          {(managed
            ? (["google-meet"] as const)
            : (["google-meet", "slack"] as const)
          ).map((integrationId) => {
            const connection = account.connections.find(
              (connection) => connection.integrationId === integrationId,
            );
            const isConnected =
              connection?.status === "connected" &&
              (!managed ||
                integrationId !== "google-meet" ||
                connected("google-calendar"));
            return (
              <Button
                key={integrationId}
                variant="outline"
                disabled={
                  isConnected || openingAction !== null || (enabled && !managed)
                }
                onClick={() =>
                  openIntegration({
                    nangoIntegrationId: integrationId,
                    connectionId: connection?.connectionId,
                    action: connection ? "reconnect" : "connect",
                  })
                }
              >
                {isConnected
                  ? `${integrationId === "google-meet" ? "Google" : "Slack"} connected`
                  : integrationId === "google-meet"
                    ? managed
                      ? "Connect Google"
                      : "Connect Google Meet"
                    : "Connect Slack"}
              </Button>
            );
          })}
        </div>
        {enabled &&
        !managed &&
        ["google-meet", "slack"].some(
          (integrationId) =>
            !account.connections.some(
              (connection) =>
                connection.integrationId === integrationId &&
                connection.status === "connected",
            ),
        ) ? (
          <p className="text-muted-foreground">
            Turn off sharing before connecting another meeting app.
          </p>
        ) : null}
        {config.error || update.error ? (
          <p role="alert">
            Sharing settings could not be saved. Local recording is unaffected.
          </p>
        ) : null}
      </div>
      <Button
        className="bg-accent text-accent-foreground hover:bg-accent/80 self-start"
        variant="outline"
        disabled={config.isPending || update.isPending || (!enabled && !ready)}
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
  const quarantined = pending.filter(
    (job) =>
      job.state.startsWith("blocked_") &&
      job.state !== "blocked_identity" &&
      job.state !== "blocked_timeline",
  );
  const retryable = pending.filter((job) => !quarantined.includes(job));
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
        Meeting identity is unverified. Uploads will retry.
      </p>
    ) : null;
  return (
    <div
      className="text-muted-foreground bg-card flex flex-wrap items-start justify-between gap-3 rounded-lg border px-4 py-3 text-xs leading-relaxed"
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
