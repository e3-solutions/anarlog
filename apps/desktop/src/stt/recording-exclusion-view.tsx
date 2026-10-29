import { Info } from "@anlg/ui/components/icons";
import { Button } from "@anlg/ui/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@anlg/ui/components/ui/popover";

export function RecordingExclusionView({
  excluded,
  roomHold = false,
  unidentified = false,
  busy = false,
  error,
  onAction,
}: {
  excluded: boolean;
  roomHold?: boolean;
  unidentified?: boolean;
  busy?: boolean;
  error?: string;
  onAction: () => void;
}) {
  return (
    <div
      className="flex max-w-72 flex-col items-end gap-1"
      data-tauri-drag-region="false"
    >
      <div className="flex shrink-0 items-center gap-1">
        {excluded ? (
          <span
            role="status"
            className="text-muted-foreground px-1 text-xs whitespace-nowrap"
          >
            Recording off
          </span>
        ) : null}
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={busy}
          aria-label={
            excluded ? "Resume recording" : "Don't record this meeting"
          }
          onClick={onAction}
          className="h-7 px-2 text-xs whitespace-nowrap"
        >
          {busy ? "Saving…" : excluded ? "Resume" : "Don't record this meeting"}
        </Button>
        <Popover>
          <PopoverTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-7 shrink-0"
              aria-label="Recording privacy details"
            >
              <Info className="size-3.5" />
            </Button>
          </PopoverTrigger>
          <PopoverContent align="end" className="text-xs leading-relaxed">
            <p className="text-foreground font-medium">Your recording choice</p>
            <p className="text-muted-foreground mt-1">
              Only your recorder on this device. This choice does not delete
              earlier audio or transcripts. Audio already sent may finish
              processing.
            </p>
            {excluded && roomHold ? (
              <p className="text-muted-foreground mt-2">
                No calendar occurrence was identified. This room stays off until
                you resume, including if this room is used again.
              </p>
            ) : null}
            {excluded && unidentified ? (
              <p className="text-muted-foreground mt-2">
                This call could not be identified. Recording stays off for this
                account on this device until you resume.
              </p>
            ) : null}
          </PopoverContent>
        </Popover>
      </div>
      {error ? (
        <p role="alert" className="w-full text-xs leading-snug text-red-600">
          {error}
        </p>
      ) : null}
    </div>
  );
}
