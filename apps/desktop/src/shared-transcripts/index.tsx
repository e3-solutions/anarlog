import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Streamdown } from "streamdown";

import { streamdownIcons } from "@anlg/ui/components/streamdown-icons";

import { useAuth } from "~/auth";
import { getSharedMeeting } from "~/cloud-api/client";
import type { Tab } from "~/store/zustand/tabs";

export function TabContentSharedTranscript({
  tab,
}: {
  tab: Extract<Tab, { type: "shared_transcripts" }>;
}) {
  const { session } = useAuth();
  const [view, setView] = useState<"notes" | "transcript">("notes");
  const meeting = useQuery({
    queryKey: ["shared-transcript", session?.user.id, tab.id],
    queryFn: () => getSharedMeeting(tab.id),
    enabled: Boolean(session && !session.user.is_anonymous),
    staleTime: 30_000,
  });

  if (!session || session.user.is_anonymous) {
    return (
      <div className="p-8">Sign in to see shared meeting transcripts.</div>
    );
  }
  if (meeting.isLoading) {
    return <div className="p-8">Loading shared meeting…</div>;
  }
  if (meeting.error || !meeting.data) {
    return (
      <div className="p-8">
        This shared meeting is unavailable. Your workspace access may have
        changed.
      </div>
    );
  }

  const documents = [
    ...meeting.data.summaries.map((summary) => ({
      id: summary.id,
      title: summary.title || "Summary",
      markdown: summary.markdown,
    })),
    ...(meeting.data.note
      ? [{ id: "note", title: "Notes", markdown: meeting.data.note.markdown }]
      : []),
  ];
  const transcript = meeting.data.transcripts
    .map((part) => part.text)
    .filter(Boolean)
    .join("\n\n");

  return (
    <main className="h-full overflow-y-auto px-8 py-8">
      <div className="mx-auto max-w-3xl">
        <p className="text-muted-foreground text-xs">Shared E3 meeting</p>
        <h1 className="mt-2 text-3xl font-semibold">
          {meeting.data.title || "Untitled meeting"}
        </h1>
        <div className="mt-6 flex gap-2 border-b pb-3">
          <button
            type="button"
            aria-current={view === "notes" ? "page" : undefined}
            onClick={() => setView("notes")}
            className="aria-[current=page]:bg-accent rounded-md px-3 py-1.5 text-sm"
          >
            Notes
          </button>
          <button
            type="button"
            aria-current={view === "transcript" ? "page" : undefined}
            onClick={() => setView("transcript")}
            className="aria-[current=page]:bg-accent rounded-md px-3 py-1.5 text-sm"
          >
            Full transcript
          </button>
        </div>
        {view === "notes" ? (
          documents.length ? (
            <div className="mt-6 space-y-8">
              {documents.map((document) => (
                <section key={document.id}>
                  <h2 className="mb-3 text-lg font-medium">{document.title}</h2>
                  <Streamdown
                    className="text-sm leading-relaxed"
                    icons={streamdownIcons}
                  >
                    {document.markdown}
                  </Streamdown>
                </section>
              ))}
            </div>
          ) : (
            <p className="text-muted-foreground mt-6">No notes yet.</p>
          )
        ) : (
          <div className="mt-6 text-sm leading-7 whitespace-pre-wrap">
            {transcript || "No transcript yet."}
          </div>
        )}
      </div>
    </main>
  );
}
