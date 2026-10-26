import { lazy, Suspense, useState } from "react";

import { Users } from "@anlg/ui/components/icons";
import { Button } from "@anlg/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@anlg/ui/components/ui/dialog";

import { env } from "~/env";

const SharedRecordingsAccount = lazy(async () => ({
  default: (await import("./index")).SharedRecordingsAccount,
}));

export function SharedRecordingsLauncher() {
  const [open, setOpen] = useState(false);
  if (!env.VITE_SHARED_RECORDINGS_PREVIEW_URL) return null;
  return (
    <>
      <Button
        variant="ghost"
        className="mx-2 justify-start gap-2"
        onClick={() => setOpen(true)}
      >
        <Users className="size-4" />
        Organization recordings
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="flex h-[85vh] max-w-5xl flex-col overflow-hidden">
          <DialogHeader>
            <DialogTitle>Organization recordings</DialogTitle>
            <DialogDescription>
              One shared recording and transcript for each meeting. Local
              recording continues independently.
            </DialogDescription>
          </DialogHeader>
          {open ? (
            <Suspense fallback={null}>
              <SharedRecordingsAccount />
            </Suspense>
          ) : null}
        </DialogContent>
      </Dialog>
    </>
  );
}
