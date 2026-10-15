import { useQuery } from "@tanstack/react-query";

import { useAuth } from "~/auth";
import { listSharedMeetings } from "~/cloud-api/client";
import { useTabs } from "~/store/zustand/tabs";

export function SharedTranscriptsNav() {
  const { session } = useAuth();
  const viewerId =
    session && !session.user.is_anonymous ? session.user.id : null;
  const currentTab = useTabs((state) => state.currentTab);
  const openCurrent = useTabs((state) => state.openCurrent);
  const meetings = useQuery({
    queryKey: ["shared-transcripts", viewerId],
    queryFn: listSharedMeetings,
    enabled: Boolean(viewerId),
    staleTime: 30_000,
    refetchInterval: 60_000,
  });

  if (!viewerId) return null;
  return (
    <section className="mt-4 border-t px-2 pt-4">
      <h2 className="text-muted-foreground px-1 text-xs font-medium tracking-wide uppercase">
        Shared transcripts
      </h2>
      {meetings.isLoading ? (
        <p className="text-muted-foreground px-1 py-3 text-sm">Loading…</p>
      ) : meetings.error ? (
        <p className="text-muted-foreground px-1 py-3 text-sm">
          Shared transcripts are unavailable.
        </p>
      ) : meetings.data?.length ? (
        <div className="mt-2 space-y-1">
          {meetings.data.map((item) => (
            <button
              key={item.share_id}
              type="button"
              aria-current={
                currentTab?.type === "shared_transcripts" &&
                currentTab.id === item.share_id
                  ? "page"
                  : undefined
              }
              onClick={() =>
                openCurrent({ type: "shared_transcripts", id: item.share_id })
              }
              className="hover:bg-accent/50 aria-[current=page]:bg-accent w-full truncate rounded-lg px-2 py-2 text-left text-sm"
            >
              {item.meeting.title || "Untitled meeting"}
            </button>
          ))}
        </div>
      ) : (
        <p className="text-muted-foreground px-1 py-3 text-sm">
          No shared transcripts yet.
        </p>
      )}
    </section>
  );
}
