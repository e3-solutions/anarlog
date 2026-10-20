import { Trans } from "@lingui/react/macro";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { Button } from "@anlg/ui/components/ui/button";
import { cn } from "@anlg/utils";

import {
  listSharedRecordings,
  readSharedRecording,
  readSharedRecordingAccount,
  transcriptText,
  type SharedRecordingAccount,
  type SharedRecordingRevision,
} from "./client";
import { SharedRecordingPreferences, SharedRecordingQueue } from "./lifecycle";
import { downloadSharedRecordingAudio } from "./native";

import { useAuth } from "~/auth";

export function SharedRecordingsAccount() {
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  const session = auth.session;
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  const account = useQuery({
    queryKey: ["shared-recordings", session?.user.id, "account"],
    queryFn: async () => {
      const session = await getSessionForRequest();
      if (!session) throw new Error("Sign in to view shared recordings.");
      const result = await readSharedRecordingAccount(session.access_token);
      if (result.userId !== session.user.id)
        throw new Error("Account changed. Reopen shared recordings.");
      return result;
    },
    enabled: !!session && session.user.is_anonymous !== true,
    retry: false,
  });
  const signIn = useMutation({ mutationFn: () => auth.signIn() });
  if (!session || session.user.is_anonymous === true) {
    return (
      <div className="p-6">
        <Button disabled={signIn.isPending} onClick={() => signIn.mutate()}>
          <Trans>Sign in</Trans>
        </Button>
      </div>
    );
  }
  if (account.error)
    return (
      <ErrorMessage
        error={account.error}
        retry={() => void account.refetch()}
      />
    );
  if (!account.data)
    return (
      <p className="p-6 text-sm">
        <Trans>Loading organizations…</Trans>
      </p>
    );
  return (
    <OrganizationRecordings key={session.user.id} account={account.data} />
  );
}

function OrganizationRecordings({
  account,
}: {
  account: SharedRecordingAccount;
}) {
  const [organizationId, setOrganizationId] = useState(
    account.workspaces[0]?.id ?? "",
  );
  const [selected, setSelected] = useState<string | null>(null);
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  const recordings = useQuery({
    queryKey: ["shared-recordings", account.userId, organizationId, "list"],
    queryFn: async () => {
      const session = await getSessionForRequest();
      if (!session || session.user.id !== account.userId)
        throw new Error("Account changed.");
      return listSharedRecordings(session.access_token, organizationId);
    },
    enabled: !!organizationId,
    refetchInterval: 10_000,
    retry: false,
  });
  if (!account.workspaces.length) {
    return (
      <p className="p-6 text-sm">
        <Trans>
          Your account has no organization access. Ask an organization
          administrator to add you.
        </Trans>
      </p>
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4">
      <label className="flex items-center gap-3 text-sm">
        <Trans>Organization</Trans>
        <select
          aria-label="Organization"
          className="bg-background rounded-md border p-2"
          value={organizationId}
          onChange={(event) => {
            setOrganizationId(event.target.value);
            setSelected(null);
          }}
        >
          {account.workspaces.map((workspace) => (
            <option key={workspace.id} value={workspace.id}>
              {workspace.name} · {workspace.role}
            </option>
          ))}
        </select>
      </label>
      <SharedRecordingPreferences
        account={account}
        organizationId={organizationId}
      />
      <SharedRecordingQueue userId={account.userId} />
      <div className="flex min-h-0 flex-1 gap-4 border-t pt-4">
        <div className="w-64 shrink-0 overflow-y-auto border-r pr-3">
          {recordings.error ? (
            <ErrorMessage
              error={recordings.error}
              retry={() => void recordings.refetch()}
            />
          ) : null}
          {recordings.isPending ? (
            <p className="text-sm">
              <Trans>Loading meetings…</Trans>
            </p>
          ) : null}
          {recordings.data?.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              <Trans>
                No shared recordings yet. Enable sharing before joining a
                supported meeting.
              </Trans>
            </p>
          ) : null}
          {recordings.data?.map((item) => (
            <button
              type="button"
              key={item.occurrenceId}
              aria-current={selected === item.occurrenceId ? "page" : undefined}
              onClick={() => setSelected(item.occurrenceId)}
              className={cn([
                "mb-1 w-full rounded-md p-3 text-left text-sm",
                selected === item.occurrenceId
                  ? "bg-accent"
                  : "hover:bg-accent/50",
              ])}
            >
              <span className="block font-medium">
                {item.occurrence.provider === "google_meet"
                  ? "Google Meet"
                  : "Slack huddle"}
              </span>
              <span className="text-muted-foreground block truncate text-xs">
                {item.occurrence.providerOccurrenceId}
              </span>
              <span className="text-muted-foreground block text-xs">
                {item.captures.filter((capture) => !capture.deleted).length}{" "}
                <Trans>sources</Trans> ·{" "}
                {item.current?.transcriptStatus ?? "awaiting audio"}
              </span>
            </button>
          ))}
        </div>
        <div className="min-w-0 flex-1 overflow-y-auto">
          {selected ? (
            <RecordingDetail
              key={`${account.userId}:${organizationId}:${selected}`}
              userId={account.userId}
              organizationId={organizationId}
              occurrenceId={selected}
            />
          ) : (
            <p className="text-muted-foreground p-4 text-sm">
              <Trans>
                Select a meeting to play its shared recording and read its
                transcript.
              </Trans>
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

function RecordingDetail({
  userId,
  organizationId,
  occurrenceId,
}: {
  userId: string;
  organizationId: string;
  occurrenceId: string;
}) {
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  const queryClient = useQueryClient();
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  const detail = useQuery({
    queryKey: ["shared-recordings", userId, organizationId, occurrenceId],
    queryFn: async () => {
      const session = await getSessionForRequest();
      if (!session || session.user.id !== userId)
        throw new Error("Account changed.");
      return readSharedRecording(
        session.access_token,
        organizationId,
        occurrenceId,
      );
    },
    refetchInterval: 10_000,
    retry: false,
  });
  if (detail.error)
    return (
      <ErrorMessage error={detail.error} retry={() => void detail.refetch()} />
    );
  if (!detail.data)
    return (
      <p className="text-sm">
        <Trans>Loading recording…</Trans>
      </p>
    );
  const { current, captures, inputsChanged } = detail.data;
  return (
    <div className="space-y-4 pr-2">
      <h2 className="text-lg font-medium">
        {detail.data.occurrence.provider === "google_meet"
          ? "Google Meet"
          : "Slack huddle"}
      </h2>
      {inputsChanged ? (
        <p className="text-muted-foreground text-sm">
          <Trans>
            New audio is waiting for processing. The published audio and
            transcript below belong to the same revision.
          </Trans>
        </p>
      ) : null}
      {current ? (
        <>
          <RecordingAudio
            key={current.revision}
            userId={userId}
            organizationId={organizationId}
            occurrenceId={occurrenceId}
            revision={current}
          />
          <p className="text-muted-foreground text-xs">
            <Trans>Revision</Trans> {current.number} ·{" "}
            {formatTime(
              current.manifest.duration_frames / current.manifest.sample_rate,
            )}{" "}
            · {current.transcriptStatus}
          </p>
          <div className="text-sm leading-relaxed whitespace-pre-wrap">
            {transcriptText(current) ||
              (current.transcript
                ? "No speech was returned for this recording."
                : "Transcription is pending. The audio is retained.")}
          </div>
          <details className="text-sm">
            <summary className="cursor-pointer">
              <Trans>Audio sources and gaps</Trans>
            </summary>
            <p className="text-muted-foreground mt-2 text-xs">
              <Trans>
                Device clocks are timing hints. Another person's audio fills
                gaps only when overlapping audio confirms alignment. Unresolved
                gaps remain silent.
              </Trans>
            </p>
            <ul className="mt-2 space-y-1 text-xs">
              {current.manifest.selections.map((selection) => (
                <li key={selection.start_frame}>
                  {formatTime(
                    selection.start_frame / current.manifest.sample_rate,
                  )}
                  –
                  {formatTime(
                    selection.end_frame / current.manifest.sample_rate,
                  )}{" "}
                  ·{" "}
                  {selection.capture_id
                    ? (captures.find(
                        (capture) => capture.captureId === selection.capture_id,
                      )?.metadata.uploaderId ?? selection.capture_id)
                    : "Missing audio · silence"}
                </li>
              ))}
            </ul>
          </details>
        </>
      ) : (
        <p className="text-muted-foreground text-sm">
          <Trans>
            Audio is being collected. No canonical recording has been published
            yet.
          </Trans>
        </p>
      )}
      <Button
        variant="outline"
        onClick={() =>
          void queryClient.invalidateQueries({
            queryKey: ["shared-recordings", userId, organizationId],
          })
        }
      >
        <Trans>Refresh</Trans>
      </Button>
    </div>
  );
}

function RecordingAudio({
  userId,
  organizationId,
  occurrenceId,
  revision,
}: {
  userId: string;
  organizationId: string;
  occurrenceId: string;
  revision: SharedRecordingRevision;
}) {
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  const audio = useQuery({
    queryKey: [
      "shared-recording-audio",
      userId,
      organizationId,
      occurrenceId,
      revision.revision,
      revision.audioSha256,
    ],
    queryFn: async () => {
      const session = await getSessionForRequest();
      if (!session || session.user.id !== userId)
        throw new Error("Account changed.");
      return downloadSharedRecordingAudio({
        userId,
        organizationId,
        occurrenceId,
        revision: revision.revision,
        audioSha256: revision.audioSha256,
        accessToken: session.access_token,
      });
    },
    retry: false,
    gcTime: 0,
  });
  if (audio.error)
    return (
      <ErrorMessage error={audio.error} retry={() => void audio.refetch()} />
    );
  return audio.data ? (
    <audio className="w-full" controls preload="metadata" src={audio.data}>
      <track kind="captions" />
    </audio>
  ) : (
    <p className="text-muted-foreground text-sm">
      <Trans>Loading shared audio…</Trans>
    </p>
  );
}

function ErrorMessage({ error, retry }: { error: Error; retry: () => void }) {
  return (
    <div role="alert" className="space-y-2 p-3 text-sm">
      <p>{error.message}</p>
      <Button variant="outline" size="sm" onClick={retry}>
        <Trans>Retry</Trans>
      </Button>
    </div>
  );
}

function formatTime(seconds: number) {
  return `${Math.floor(seconds / 60)}:${Math.floor(seconds % 60)
    .toString()
    .padStart(2, "0")}`;
}
