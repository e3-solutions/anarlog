import { Button } from "@anlg/ui/components/ui/button";

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
      className="border-border bg-muted/30 flex flex-wrap items-center justify-between gap-3 rounded-lg border px-3 py-2.5 text-sm"
      data-tauri-drag-region="false"
    >
      <div className="min-w-0 flex-1 basis-48">
        <p className="text-foreground font-medium">
          {excluded
            ? "Recording is off for this meeting"
            : "Keep this meeting private"}
        </p>
        <p className="text-muted-foreground mt-0.5 text-xs leading-relaxed">
          Only your recorder on this device. This choice does not delete earlier
          audio or transcripts. Audio already sent may finish processing.
        </p>
        {excluded && roomHold ? (
          <p className="text-muted-foreground mt-1 text-xs leading-relaxed">
            No calendar occurrence was identified. This room stays off until you
            resume, including if this room is used again.
          </p>
        ) : null}
        {excluded && unidentified ? (
          <p className="text-muted-foreground mt-1 text-xs leading-relaxed">
            This call could not be identified. Recording stays off for this
            account on this device until you resume.
          </p>
        ) : null}
        {error ? (
          <p role="alert" className="mt-1 text-xs text-red-600">
            {error}
          </p>
        ) : null}
      </div>
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={busy}
        onClick={onAction}
      >
        {busy
          ? "Saving…"
          : excluded
            ? "Resume recording"
            : "Don't record this meeting"}
      </Button>
    </div>
  );
}
