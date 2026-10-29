import { useMutation, useQuery } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";

import { Button } from "@anlg/ui/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@anlg/ui/components/ui/select";
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

const providerLabels = {
  google_meet: "Google Meet",
  slack_huddle: "Slack huddle",
  unverified: "Unverified recording",
};

export function SharedRecordingsAccount({
  settings = false,
}: {
  settings?: boolean;
}) {
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
    <OrganizationRecordings
      key={session.user.id}
      account={account.data}
      settings={settings}
    />
  );
}

function OrganizationRecordings({
  account,
  settings,
}: {
  account: SharedRecordingAccount;
  settings: boolean;
}) {
  const [organizationId, setOrganizationId] = useState(
    account.policy?.workspaceId ?? account.workspaces[0]?.id ?? "",
  );
  const [selected, setSelected] = useState<string | null>(null);
  const organizationPickerId = useId();
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
    enabled: !!organizationId && !settings,
    refetchInterval: 10_000,
    retry: false,
  });
  const personalRecordings = (recordings.data ?? [])
    .map((item) => ({
      ...item,
      captures: item.captures.filter(
        (capture) => capture.metadata.uploaderId === account.userId,
      ),
    }))
    .filter((item) => item.captures.some((capture) => !capture.deleted));
  const selectedOccurrenceId =
    personalRecordings.find((item) => item.occurrenceId === selected)
      ?.occurrenceId ??
    personalRecordings[0]?.occurrenceId ??
    null;
  if (!account.workspaces.length) {
    return (
      <p className="p-6 text-sm">
        Your account has no organization access. Ask an organization
        administrator to add you.
      </p>
    );
  }
  const organizationPicker =
    account.workspaces.length > 1 ? (
      <div className="flex min-w-0 items-center gap-3 text-sm">
        <label htmlFor={organizationPickerId}>Organization</label>
        <Select
          value={organizationId}
          onValueChange={(value) => {
            setOrganizationId(value);
            setSelected(null);
          }}
        >
          <SelectTrigger
            id={organizationPickerId}
            className="hover:bg-accent/50 data-[state=open]:bg-accent/50 w-64 max-w-full cursor-pointer transition-colors motion-reduce:transition-none"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent
            align="start"
            sideOffset={6}
            className="motion-reduce:animate-none"
          >
            {account.workspaces.map((workspace) => (
              <SelectItem
                key={workspace.id}
                value={workspace.id}
                className="cursor-pointer"
              >
                {workspace.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    ) : null;
  if (settings)
    return (
      <div className="flex flex-col gap-4">
        {organizationPicker}
        <SharedRecordingPreferences
          account={account}
          organizationId={organizationId}
        />
        <SharedRecordingQueue userId={account.userId} />
      </div>
    );
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {organizationPicker ? (
        <div className="shrink-0 border-b px-6 py-3">{organizationPicker}</div>
      ) : null}
      <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
        <nav
          aria-label="Meetings"
          className="bg-muted/30 max-h-48 shrink-0 overflow-y-auto border-b p-2 sm:max-h-none sm:w-64 sm:border-r sm:border-b-0"
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
          {!recordings.isPending && personalRecordings.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              Your recordings will appear here.
            </p>
          ) : null}
          {personalRecordings.map((item) => {
            const sources = groupRecordings(item.captures);
            return (
              <button
                type="button"
                key={item.occurrenceId}
                aria-current={
                  selectedOccurrenceId === item.occurrenceId
                    ? "page"
                    : undefined
                }
                onClick={() => setSelected(item.occurrenceId)}
                className={cn([
                  "focus-visible:outline-ring mb-1 flex w-full min-w-0 cursor-pointer flex-col gap-1 rounded-lg px-3 py-2 text-left text-sm transition-colors focus-visible:outline-2 focus-visible:outline-offset-2",
                  selectedOccurrenceId === item.occurrenceId
                    ? "bg-accent text-accent-foreground"
                    : "hover:bg-accent/50",
                ])}
              >
                <span className="block font-medium">
                  {providerLabels[item.occurrence.provider]}
                </span>
                <span className="text-muted-foreground block w-full truncate text-xs">
                  {sources
                    .flat()
                    .map(transcriptText)
                    .find(Boolean)
                    ?.replace(/\s+/g, " ")
                    .slice(0, 120) || "No transcript yet"}
                </span>
                <span className="text-muted-foreground text-xs">
                  {sources.length}{" "}
                  {sources.length === 1 ? "recording" : "recordings"}
                </span>
              </button>
            );
          })}
        </nav>
        <div className="bg-card min-w-0 flex-1 overflow-y-auto px-6 py-6">
          {selectedOccurrenceId ? (
            <RecordingDetail
              key={`${account.userId}:${organizationId}:${selectedOccurrenceId}`}
              userId={account.userId}
              organizationId={organizationId}
              occurrenceId={selectedOccurrenceId}
            />
          ) : (
            <div className="mx-auto flex h-full max-w-sm flex-col justify-center gap-2 pb-12">
              <h2 className="text-xl font-semibold tracking-tight">
                Your recordings
              </h2>
              <p className="text-muted-foreground text-sm leading-relaxed">
                Choose a meeting to listen or read your transcript.
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
  const [view, setView] = useState<"recordings" | "transcript">("recordings");
  const viewId = useId();
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
  if (!detail.data) return <p className="text-sm">Loading meeting…</p>;
  const sources = groupRecordings(
    detail.data.captures.filter(
      (capture) => capture.metadata.uploaderId === userId,
    ),
  );
  return (
    <div className="mx-auto max-w-3xl space-y-6">
      <div className="space-y-1">
        <h2 className="text-xl font-semibold">
          {providerLabels[detail.data.occurrence.provider]}
        </h2>
        {detail.data.occurrence.provider === "unverified" ? (
          <p className="text-muted-foreground text-sm">
            Meeting identity could not be verified.
          </p>
        ) : null}
        {sources.length > 1 ? (
          <p className="text-muted-foreground text-sm">
            {sources.length} recordings
          </p>
        ) : null}
      </div>
      <div
        role="tablist"
        aria-label="Meeting view"
        className="bg-foreground/10 dark:bg-accent/55 rounded-pill flex w-fit gap-0.5 p-0.5 [corner-shape:round]"
      >
        {(["recordings", "transcript"] as const).map((tab) => (
          <button
            key={tab}
            id={`${viewId}-${tab}`}
            type="button"
            role="tab"
            data-view={tab}
            aria-selected={view === tab}
            aria-controls={`${viewId}-panel`}
            tabIndex={view === tab ? 0 : -1}
            className={cn([
              "rounded-pill focus-visible:outline-ring h-7 cursor-pointer px-3 text-xs font-medium transition-colors [corner-shape:round] focus-visible:outline-2 focus-visible:outline-offset-2",
              view === tab
                ? "bg-card text-foreground shadow-xs"
                : "text-muted-foreground hover:text-foreground",
            ])}
            onClick={() => setView(tab)}
            onKeyDown={(event) => {
              if (
                !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)
              )
                return;
              event.preventDefault();
              const next =
                event.key === "Home"
                  ? "recordings"
                  : event.key === "End"
                    ? "transcript"
                    : tab === "recordings"
                      ? "transcript"
                      : "recordings";
              setView(next);
              event.currentTarget.parentElement
                ?.querySelector<HTMLButtonElement>(`[data-view="${next}"]`)
                ?.focus();
            }}
          >
            {tab === "recordings" ? "Recordings" : "Transcript"}
          </button>
        ))}
      </div>
      <ul
        id={`${viewId}-panel`}
        role="tabpanel"
        tabIndex={0}
        aria-labelledby={`${viewId}-${view}`}
        className="space-y-6"
      >
        {sources.map((parts) => (
          <li
            key={parts[0].captureId}
            className="space-y-4 border-b pb-6 last:border-b-0"
          >
            <div className="flex items-baseline justify-between gap-3">
              <h3 className="text-sm font-medium">Your recording</h3>
              <p className="text-muted-foreground text-xs tabular-nums">
                {formatTime(
                  parts.reduce((frames, part) => frames + part.frameCount, 0) /
                    16000,
                )}
              </p>
            </div>
            {parts.map((capture, partIndex) => (
              <div
                key={`${capture.captureId}:${capture.metadata.sha256}`}
                className="space-y-2"
              >
                {parts.length > 1 ? (
                  <p className="text-muted-foreground text-xs">
                    Part {partIndex + 1}
                  </p>
                ) : null}
                {view === "recordings" ? (
                  <RecordingAudio
                    userId={userId}
                    organizationId={organizationId}
                    occurrenceId={occurrenceId}
                    source={capture}
                  />
                ) : (
                  <p className="text-sm leading-7 whitespace-pre-wrap select-text">
                    {transcriptText(capture) ||
                      (capture.transcriptStatus === "ready"
                        ? "No speech detected."
                        : capture.transcriptStatus === "error"
                          ? "Transcription failed. You can still listen to the recording."
                          : "Transcribing…")}
                  </p>
                )}
                {capture.metadata.audioTimelineVerified === false ? (
                  <p className="text-muted-foreground text-sm">
                    Audio timing could not be verified.
                  </p>
                ) : capture.metadata.gaps.length ? (
                  <p className="text-muted-foreground text-sm">
                    Some audio is missing from this recording.
                  </p>
                ) : null}
              </div>
            ))}
          </li>
        ))}
        {sources.length === 0 ? (
          <li className="text-muted-foreground text-sm">
            No recordings available.
          </li>
        ) : null}
      </ul>
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
      className="accent-primary w-full"
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
    <p className="text-muted-foreground text-sm">Loading audio…</p>
  ) : (
    <Button variant="outline" size="sm" onClick={() => void audio.refetch()}>
      Listen
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

function groupRecordings(captures: SharedRecordingSource[]) {
  // Upload parts share this identity; other devices and recording sessions stay separate.
  const groups = new Map<string, SharedRecordingSource[]>();
  for (const capture of captures) {
    if (capture.deleted) continue;
    const { uploaderId, deviceId, localCaptureId } = capture.metadata;
    const key = JSON.stringify([uploaderId, deviceId, localCaptureId]);
    const parts = groups.get(key) ?? [];
    parts.push(capture);
    groups.set(key, parts);
  }
  return [...groups.values()].map((parts) =>
    parts.sort(
      (left, right) => left.metadata.offsetFrames - right.metadata.offsetFrames,
    ),
  );
}
