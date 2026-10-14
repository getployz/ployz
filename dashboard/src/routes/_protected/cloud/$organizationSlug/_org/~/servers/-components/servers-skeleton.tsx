import { Skeleton } from "#/components/ui/skeleton";
import { SERVER_LIST, SERVER_ROW_GRID } from "./server-row";

export function ServersSkeleton() {
  return (
    <div role="status" aria-label="Servers loading" className={SERVER_LIST}>
      {[0, 1, 2].map((row) => (
        <div key={row} className={SERVER_ROW_GRID}>
          <Skeleton className="size-2 rounded-full" />
          <Skeleton className="h-4 w-32 max-w-full" />
          <Skeleton className="col-2 row-2 h-4 w-40 max-w-full @xl:col-auto @xl:row-auto" />
        </div>
      ))}
    </div>
  );
}
