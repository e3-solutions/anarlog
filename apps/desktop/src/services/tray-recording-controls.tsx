import { useQuery } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { useRef } from "react";

import { toast } from "@anlg/ui/components/ui/toast";
import { useMountEffect } from "@anlg/ui/hooks/use-mount-effect";

import { useOptionalAuth } from "~/auth";
import { listenerStore } from "~/store/zustand/listener/instance";
import {
  replayNativeRecordingExclusions,
  type NativeMeetingContext,
} from "~/stt/meeting-recording-control";
import { resumeMeetingRecording } from "~/stt/recording-exclusion-actions";

export function TrayRecordingControls() {
  const userId = useOptionalAuth()?.session?.user.id;
  return userId ? (
    <AccountRecordingControls key={userId} userId={userId} />
  ) : null;
}

export async function handleMeetingRecordingControl(
  event: {
    action: "excluded" | "resume";
    context: NativeMeetingContext;
    error: string | null;
  },
  userId: string,
  isCurrent: () => boolean,
) {
  if (!isCurrent() || event.context.userId !== userId) return;
  // Events wake the UI; native durable holds are authoritative even if an event was missed.
  const holds = await replayNativeRecordingExclusions(userId);
  if (!isCurrent()) return;
  const held = holds.find(
    (context) =>
      context.revision === event.context.revision &&
      context.sessionId === event.context.sessionId,
  );
  if (!held) return;
  if (event.action === "excluded") {
    if (listenerStore.getState().live.sessionId === held.sessionId)
      listenerStore.getState().stop();
    if (event.error)
      throw new Error(
        "Recording stop could not be confirmed. Quit this app to stop capture before trying again.",
      );
  } else {
    await resumeMeetingRecording({
      userId,
      occurrence: held.occurrence,
      expected: held,
      isCurrent,
    });
  }
}

function AccountRecordingControls({ userId }: { userId: string }) {
  const mounted = useRef(true);
  const isCurrent = () => mounted.current;
  useQuery({
    queryKey: ["native-recording-exclusions", userId],
    queryFn: () => replayNativeRecordingExclusions(userId),
    refetchInterval: 2000,
    retry: false,
  });
  useMountEffect(() => {
    mounted.current = true;
    const unlisten = listen<
      Parameters<typeof handleMeetingRecordingControl>[0]
    >("e3:meeting-recording-control", ({ payload }) => {
      void handleMeetingRecordingControl(payload, userId, isCurrent).catch(
        (error) => {
          if (isCurrent())
            toast.error("Recording control needs attention", {
              description:
                error instanceof Error
                  ? error.message
                  : "Reopen the app to check recording status.",
            });
        },
      );
    });
    return () => {
      mounted.current = false;
      void unlisten.then((remove) => remove()).catch(() => {});
    };
  });
  return null;
}
