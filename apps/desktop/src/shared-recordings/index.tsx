import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";

import { Button } from "@anlg/ui/components/ui/button";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";
import { cn } from "@anlg/utils";

import {
  listSharedRecordings,
  downloadSharedRecordingSource,
  readSharedRecording,
  readSharedRecordingAccount,
  transcriptText,
  type SharedRecordingAccount,
  type SharedRecordingSource,
} from "./client";
import { SharedRecordingPreferences, SharedRecordingQueue } from "./settings";

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
    refetchInterval: 5_000,
    retry: false,
  });
  const signIn = useMutation({ mutationFn: () => auth.signIn() });
  if (!session || session.user.is_anonymous === true) {
    return (
      <div className="p-6">
        <Button disabled={signIn.isPending} onClick={() => signIn.mutate()}>
          Sign in
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
    return <p className="p-6 text-sm">Loading organizations…</p>;
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
        Your account has no organization access. Ask an organization
        administrator to add you.
      </p>
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4">
      <label className="flex items-center gap-3 text-sm">
        Organization
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
            <p className="text-sm">Loading meetings…</p>
          ) : null}
          {recordings.data?.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              No shared recordings yet. Enable sharing before joining a
              supported meeting.
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
                sources ·{" "}
                {
                  item.captures.filter(
                    (capture) =>
                      !capture.deleted && capture.transcriptStatus === "ready",
                  ).length
                }{" "}
                transcribed
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
              Select a meeting to read its source transcripts and play the
              original audio recordings.
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
  if (!detail.data) return <p className="text-sm">Loading recording…</p>;
  const { captures } = detail.data;
  return (
    <div className="space-y-4 pr-2">
      <h2 className="text-lg font-medium">
        {detail.data.occurrence.provider === "google_meet"
          ? "Google Meet"
          : "Slack huddle"}
      </h2>
      <h3 className="font-medium">Original audio and transcripts</h3>
      <p className="text-muted-foreground text-sm">
        Each recording and its transcript stay separate. Start times are device
        clock estimates.
      </p>
      <ul className="space-y-4">
        {captures
          .filter((capture) => !capture.deleted)
          .map((capture) => (
            <li
              key={`${capture.captureId}:${capture.metadata.sha256}`}
              className="space-y-2 border-t pt-3"
            >
              <p className="text-sm">
                {capture.metadata.uploaderId} · source{" "}
                {(capture.metadata.segmentSequence ?? 0) + 1} · starts{" "}
                {formatTime(capture.metadata.offsetFrames / 16000)} ·{" "}
                {formatTime(capture.frameCount / 16000)} long
              </p>
              {capture.metadata.gaps.length ? (
                <p className="text-muted-foreground text-xs">
                  Missing audio within this source:{" "}
                  {capture.metadata.gaps
                    .map(
                      (gap) =>
                        `${formatTime(gap.start / 16000)}–${formatTime(gap.end / 16000)}`,
                    )
                    .join(", ")}
                </p>
              ) : null}
              <details className="text-sm">
                <summary>
                  Source transcript · {capture.transcriptStatus}
                </summary>
                <p className="pt-2 whitespace-pre-wrap">
                  {transcriptText(capture) ||
                    (capture.transcriptStatus === "ready"
                      ? "No speech was returned for this source."
                      : capture.transcriptStatus === "error"
                        ? "Source transcription failed. Original audio is retained."
                        : "Source transcription is pending. Original audio is retained.")}
                </p>
              </details>
              <RecordingAudio
                userId={userId}
                organizationId={organizationId}
                occurrenceId={occurrenceId}
                source={capture}
              />
            </li>
          ))}
      </ul>
      <Button
        variant="outline"
        onClick={() =>
          void queryClient.invalidateQueries({
            queryKey: ["shared-recordings", userId, organizationId],
          })
        }
      >
        Refresh
      </Button>
    </div>
  );
}

function RecordingAudio({
  userId,
  organizationId,
  occurrenceId,
  source,
}: {
  userId: string;
  organizationId: string;
  occurrenceId: string;
  source: SharedRecordingSource;
}) {
  const auth = useAuth();
  const { getSessionForRequest } = auth;
  const [mediaError, setMediaError] = useState<Error | null>(null);
  const playbackId = useId();
  const resource = useRef<{ url: string | null; disposed: boolean }>({
    url: null,
    disposed: false,
  });
  useMountEffect(() => {
    resource.current.disposed = false;
    return () => {
      resource.current.disposed = true;
      if (resource.current.url) URL.revokeObjectURL(resource.current.url);
      resource.current.url = null;
    };
  });
  // Refresh credentials at request time; user IDs define cache ownership.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps
  const audio = useQuery({
    queryKey: [
      "shared-recording-audio",
      userId,
      organizationId,
      occurrenceId,
      source.captureId,
      source.metadata.sha256,
      playbackId,
    ],
    queryFn: async ({ signal }) => {
      const session = await getSessionForRequest();
      if (!session || session.user.id !== userId)
        throw new Error("Account changed.");
      const blob = await downloadSharedRecordingSource(
        {
          userId,
          organizationId,
          occurrenceId,
          captureId: source.captureId,
          sha256: source.metadata.sha256,
          accessToken: session.access_token,
        },
        signal,
      );
      signal.throwIfAborted();
      if (resource.current.disposed)
        throw new Error("Shared audio playback was closed.");
      const url = URL.createObjectURL(blob);
      if (resource.current.url) URL.revokeObjectURL(resource.current.url);
      resource.current.url = url;
      return url;
    },
    enabled: false,
    retry: false,
    gcTime: 0,
    staleTime: Infinity,
  });
  const error = audio.error ?? mediaError;
  if (error)
    return (
      <ErrorMessage
        error={error}
        retry={() => {
          setMediaError(null);
          void audio.refetch();
        }}
      />
    );
  return audio.data && !audio.isFetching ? (
    <audio
      className="w-full"
      controls
      preload="metadata"
      src={audio.data}
      onError={(event) => {
        if (resource.current.url) URL.revokeObjectURL(resource.current.url);
        resource.current.url = null;
        setMediaError(
          new Error(
            `Shared audio could not be played (media error ${event.currentTarget.error?.code ?? "unknown"}). Retry loading the recording.`,
          ),
        );
      }}
    >
      <track kind="captions" />
    </audio>
  ) : audio.isFetching ? (
    <p className="text-muted-foreground text-sm">Loading source audio…</p>
  ) : (
    <Button variant="outline" onClick={() => void audio.refetch()}>
      Load audio
    </Button>
  );
}

function ErrorMessage({ error, retry }: { error: Error; retry: () => void }) {
  return (
    <div role="alert" className="space-y-2 p-3 text-sm">
      <p>{error.message}</p>
      <Button variant="outline" size="sm" onClick={retry}>
        Retry
      </Button>
    </div>
  );
}

function formatTime(seconds: number) {
  const sign = seconds < 0 ? "−" : "";
  seconds = Math.abs(seconds);
  return `${sign}${Math.floor(seconds / 60)}:${Math.floor(seconds % 60)
    .toString()
    .padStart(2, "0")}`;
}
