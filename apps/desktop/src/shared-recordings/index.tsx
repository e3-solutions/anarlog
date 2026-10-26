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
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-b px-6 py-3">
        <label className="flex items-center gap-3 text-sm">
          Organization
          <select
            aria-label="Organization"
            className="bg-background max-w-64 rounded-md border px-3 py-1.5 focus-visible:outline-violet-600"
            value={organizationId}
            onChange={(event) => {
              setOrganizationId(event.target.value);
              setSelected(null);
            }}
          >
            {account.workspaces.map((workspace) => (
              <option key={workspace.id} value={workspace.id}>
                {workspace.name}
              </option>
            ))}
          </select>
        </label>
        <details className="max-w-xl text-sm">
          <summary className="cursor-pointer rounded-md px-3 py-1.5 text-violet-700 hover:bg-violet-50 focus-visible:outline-violet-600 dark:text-violet-300 dark:hover:bg-violet-950">
            Sharing settings
          </summary>
          <SharedRecordingPreferences
            account={account}
            organizationId={organizationId}
          />
        </details>
      </div>
      <div className="px-6">
        <SharedRecordingQueue userId={account.userId} />
      </div>
      <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
        <nav
          aria-label="Meetings"
          className="max-h-48 shrink-0 overflow-y-auto border-b bg-violet-50/40 p-3 sm:max-h-none sm:w-60 sm:border-r sm:border-b-0 dark:bg-violet-950/10"
        >
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
                "mb-1 w-full rounded-lg p-3 text-left text-sm transition-colors focus-visible:outline-violet-600",
                selected === item.occurrenceId
                  ? "bg-violet-100 text-violet-950 dark:bg-violet-900/60 dark:text-violet-100"
                  : "hover:bg-violet-100/60 dark:hover:bg-violet-900/30",
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
        </nav>
        <div className="min-w-0 flex-1 overflow-y-auto p-6">
          {selected ? (
            <RecordingDetail
              key={`${account.userId}:${organizationId}:${selected}`}
              userId={account.userId}
              organizationId={organizationId}
              occurrenceId={selected}
            />
          ) : (
            <div className="mx-auto flex h-full max-w-sm flex-col justify-center gap-2 pb-12">
              <h2 className="text-xl font-semibold tracking-tight">
                Every perspective, saved.
              </h2>
              <p className="text-muted-foreground text-sm leading-relaxed">
                Choose a meeting to listen to each person’s recording and read
                its transcript.
              </p>
            </div>
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
    <div className="mx-auto max-w-3xl space-y-5">
      <h2 className="text-2xl font-semibold tracking-tight text-violet-950 dark:text-violet-100">
        {detail.data.occurrence.provider === "google_meet"
          ? "Google Meet"
          : "Slack huddle"}
      </h2>
      <details className="text-muted-foreground text-xs">
        <summary className="cursor-pointer">Verified meeting identity</summary>
        <p className="pt-2 font-mono break-all select-text">
          {detail.data.occurrence.providerOccurrenceId}
        </p>
      </details>
      <div className="space-y-1">
        <h3 className="font-medium">Original audio and transcripts</h3>
        <p className="text-muted-foreground text-sm">
          Each person’s audio and transcript are saved separately.
        </p>
      </div>
      <ul className="space-y-4">
        {captures
          .filter((capture) => !capture.deleted)
          .map((capture, index) => (
            <li
              key={`${capture.captureId}:${capture.metadata.sha256}`}
              className="space-y-3 border-t py-4"
            >
              <div className="flex flex-wrap items-baseline justify-between gap-2">
                <p className="text-sm font-medium">
                  Recording {index + 1} ·{" "}
                  {capture.metadata.uploaderId.slice(0, 8)}
                </p>
                <p className="text-muted-foreground text-xs tabular-nums">
                  {formatTime(capture.frameCount / 16000)} · starts at ~
                  {formatTime(capture.metadata.offsetFrames / 16000)}
                </p>
              </div>
              <details className="text-muted-foreground text-xs">
                <summary className="cursor-pointer">Recording details</summary>
                <p className="pt-2 break-all select-text">
                  Contributor: {capture.metadata.uploaderId}
                </p>
                <p>
                  Start time is estimated from the recording device’s clock.
                </p>
              </details>
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
              <details className="rounded-lg bg-violet-50/70 p-3 text-sm dark:bg-violet-950/30">
                <summary className="cursor-pointer font-medium text-violet-900 dark:text-violet-200">
                  Source transcript · {capture.transcriptStatus}
                </summary>
                <p className="pt-3 leading-relaxed whitespace-pre-wrap select-text">
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
      className="w-full accent-violet-600"
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
    <div
      role="alert"
      className="space-y-2 rounded-lg border border-rose-200 bg-rose-50 p-3 text-sm text-rose-950 dark:border-rose-900 dark:bg-rose-950/30 dark:text-rose-100"
    >
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
