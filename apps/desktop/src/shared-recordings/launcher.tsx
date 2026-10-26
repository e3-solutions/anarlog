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

const SharedRecordingsAccount = lazy(async () => ({
  default: (await import("./index")).SharedRecordingsAccount,
}));

export function SharedRecordingsLauncher() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button
        variant="ghost"
        className="mx-2 mb-2 justify-start gap-2 text-violet-700 hover:bg-violet-100 hover:text-violet-900 dark:text-violet-300 dark:hover:bg-violet-950"
        onClick={() => setOpen(true)}
      >
        <Users className="size-4" />
        Meeting library
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          overlayClassName="bg-slate-950/25"
          className="top-3 right-3 bottom-3 left-auto flex h-auto w-[calc(100vw-1.5rem)] max-w-6xl translate-x-0 translate-y-0 flex-col gap-0 overflow-hidden border-violet-200 p-0 data-[state=closed]:animate-none data-[state=open]:animate-none dark:border-violet-900"
        >
          <DialogHeader className="border-b border-violet-100 bg-violet-50 px-6 py-5 text-left dark:border-violet-950 dark:bg-violet-950/40">
            <DialogTitle className="text-xl tracking-tight text-violet-950 dark:text-violet-100">
              Meeting library
            </DialogTitle>
            <DialogDescription>
              Your team’s meeting audio and transcripts, together in one place.
            </DialogDescription>
          </DialogHeader>
          {open ? (
            <Suspense
              fallback={<p className="p-6 text-sm">Loading meeting library…</p>}
            >
              <SharedRecordingsAccount />
            </Suspense>
          ) : null}
        </DialogContent>
      </Dialog>
    </>
  );
}
