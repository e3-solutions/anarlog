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
        className="text-primary hover:bg-accent hover:text-accent-foreground mx-2 mb-2 justify-start gap-2 transition-colors duration-150"
        onClick={() => setOpen(true)}
      >
        <Users className="size-4" />
        Meeting library
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          overlayClassName="bg-black/30 backdrop-blur-[2px] motion-reduce:animate-none"
          className="top-3 right-3 bottom-3 left-auto flex h-auto w-[calc(100vw-1.5rem)] max-w-6xl translate-x-0 translate-y-0 flex-col gap-0 overflow-hidden rounded-xl p-0 data-[state=closed]:animate-none data-[state=open]:animate-none"
        >
          <DialogHeader className="border-b px-6 py-4 pr-14 text-left">
            <DialogTitle className="text-foreground text-xl font-semibold">
              Meeting library
            </DialogTitle>
            <DialogDescription>
              Meetings shared with your team.
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
