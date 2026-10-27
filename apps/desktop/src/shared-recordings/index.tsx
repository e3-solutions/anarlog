import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";

import { Button } from "@anlg/ui/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@anlg/ui/components/ui/popover";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

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
    <div className="meeting-library-content flex min-h-0 flex-1 flex-col">
      <div className="meeting-toolbar flex shrink-0 flex-wrap items-start justify-between gap-3 border-b px-6 py-4">
        <label className="flex min-w-0 items-center gap-3 text-sm font-medium">
          Organization
          <select
            aria-label="Organization"
            className="meeting-organization bg-card text-foreground h-9 max-w-64 min-w-0 rounded-lg border px-3 text-sm font-normal"
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
        <Popover>
          <PopoverTrigger asChild>
            <Button
              variant="outline"
              className="meeting-sharing-trigger text-primary gap-2"
            >
              Sharing settings
            </Button>
          </PopoverTrigger>
          <PopoverContent
            align="end"
            sideOffset={10}
            className="meeting-sharing-popover w-[min(28rem,calc(100vw-3rem))] rounded-xl p-5"
          >
            <SharedRecordingPreferences
              account={account}
              organizationId={organizationId}
            />
          </PopoverContent>
        </Popover>
      </div>
      <div className="meeting-queue px-6">
        <SharedRecordingQueue userId={account.userId} />
      </div>
      <div className="meeting-library-body flex min-h-0 flex-1 flex-col sm:flex-row">
        <nav
          aria-label="Meetings"
          className="meeting-list max-h-48 shrink-0 overflow-y-auto border-b p-3 sm:max-h-none sm:w-64 sm:border-r sm:border-b-0 lg:w-72"
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
              title={item.occurrence.providerOccurrenceId}
              className="meeting-row mb-1.5 w-full rounded-lg border border-transparent p-3 text-left text-sm"
            >
              <span className="block font-medium">
                {item.occurrence.provider === "google_meet"
                  ? "Google Meet"
                  : "Slack huddle"}
              </span>
              <span className="text-muted-foreground mt-1 block truncate font-mono text-[11px]">
                {item.occurrence.providerOccurrenceId}
              </span>
              <span className="text-muted-foreground mt-1.5 block text-xs tabular-nums">
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
        <div className="meeting-detail min-w-0 flex-1 overflow-y-auto p-5 sm:p-7">
          {selected ? (
            <RecordingDetail
              key={`${account.userId}:${organizationId}:${selected}`}
              userId={account.userId}
              organizationId={organizationId}
              occurrenceId={selected}
            />
          ) : (
            <div className="meeting-empty mx-auto flex h-full max-w-sm flex-col justify-center gap-3 pb-12">
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
    <div className="mx-auto max-w-3xl space-y-6">
      <h2 className="text-foreground text-2xl font-semibold tracking-tight">
        {detail.data.occurrence.provider === "google_meet"
          ? "Google Meet"
          : "Slack huddle"}
      </h2>
      <details className="meeting-disclosure text-muted-foreground text-xs">
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
              className="meeting-source bg-card space-y-4 rounded-xl border p-5"
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
              <details className="meeting-disclosure text-muted-foreground text-xs">
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
              <details className="meeting-transcript meeting-disclosure rounded-lg text-sm">
                <summary className="text-primary cursor-pointer font-medium">
                  Source transcript · {capture.transcriptStatus}
                </summary>
                <p className="text-foreground border-t px-4 py-4 leading-7 whitespace-pre-wrap select-text">
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
    <Button onClick={() => void audio.refetch()}>Load audio</Button>
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
