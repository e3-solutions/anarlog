import e3MarkUrl from "../../src-tauri/icons/e3/icon.svg";

export function BrandLoadingView({ detail }: { detail?: string }) {
  return (
    <div
      role="status"
      aria-label="Loading"
      data-tauri-drag-region
      className="bg-background flex h-screen w-screen items-center justify-center"
    >
      <div className="flex flex-col items-center">
        <img src={e3MarkUrl} alt="E3 Scribe" className="size-20" />
        <p className="mt-4 text-lg font-semibold tracking-tight text-violet-950 dark:text-violet-100">
          E3 Scribe
        </p>
        {detail ? (
          <p className="text-muted-foreground mt-3 max-w-64 text-center text-xs">
            {detail}
          </p>
        ) : null}
      </div>
    </div>
  );
}
