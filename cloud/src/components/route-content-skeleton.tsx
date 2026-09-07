import { Skeleton } from "#/components/ui/skeleton";

const CONTENT_ROWS = [0, 1, 2];

export function RouteContentSkeleton() {
  return (
    <div
      role="status"
      aria-label="Loading page"
      aria-busy="true"
      className="flex flex-col gap-4"
    >
      <div aria-hidden="true" className="flex flex-col gap-2">
        <Skeleton className="h-5 w-40 max-w-full" />
        <Skeleton className="h-3 w-64 max-w-full" />
      </div>
      {CONTENT_ROWS.map((row) => (
        <div
          key={row}
          aria-hidden="true"
          className="flex items-center gap-3 rounded-lg border p-3"
        >
          <Skeleton className="size-8 rounded-lg" />
          <div className="flex flex-1 flex-col gap-2">
            <Skeleton className="h-4 w-48 max-w-full" />
            <Skeleton className="h-3 w-32 max-w-full" />
          </div>
        </div>
      ))}
    </div>
  );
}
