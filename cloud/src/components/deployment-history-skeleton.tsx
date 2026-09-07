import { Skeleton } from "#/components/ui/skeleton";

const DEPLOYMENT_ROWS = [0, 1, 2, 3];

function DeploymentRowSkeleton() {
  return (
    <div aria-hidden="true" className="rounded-lg border p-3">
      <div className="flex items-center gap-3">
        <Skeleton className="h-5 w-20 rounded-full" />
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <Skeleton className="h-4 w-48 max-w-full" />
          <Skeleton className="h-3 w-32 max-w-full" />
        </div>
        <Skeleton className="size-8 rounded-lg" />
      </div>
    </div>
  );
}

export function DeploymentHistorySkeleton() {
  return (
    <div
      role="status"
      aria-label="Loading deployments"
      aria-busy="true"
      className="flex flex-col gap-4"
    >
      {DEPLOYMENT_ROWS.map((row) => (
        <DeploymentRowSkeleton key={row} />
      ))}
    </div>
  );
}
