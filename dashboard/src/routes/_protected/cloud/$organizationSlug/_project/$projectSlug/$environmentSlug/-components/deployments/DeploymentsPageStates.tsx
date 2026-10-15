import { RouteErrorAlert } from "#/components/route-error-alert";
import { Skeleton } from "#/components/ui/skeleton";

export function DeploymentsPagePending() {
  return (
    <div role="status" aria-label="Loading" className="flex flex-col gap-3 p-4">
      <Skeleton className="h-8 w-48 max-w-full" />
      {Array.from({ length: 4 }, (_, index) => <Skeleton key={index} className="h-12 w-full" />)}
    </div>
  );
}

export function DeploymentsPageError({ noun }: { noun: string }) {
  return (
    <div className="p-4">
      <RouteErrorAlert title={`${noun} couldn’t load`} description="Try loading the page again." />
    </div>
  );
}
