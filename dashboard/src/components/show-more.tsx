import { Button } from "#/components/ui/button";
import { Skeleton } from "#/components/ui/skeleton";

/** Skeleton rows standing in for a list page on its way. */
export function ListRowSkeletons() {
  return <div aria-hidden className="flex flex-col gap-3 p-2">
    {[0, 1, 2].map((row) => <div key={row} className="flex flex-col gap-1.5"><Skeleton className="h-4 w-48" /><Skeleton className="h-3 w-32" /></div>)}
  </div>;
}

/** The end of a paged list: Show more while another page exists, skeleton rows while it loads. */
export function ShowMore({ hasMore, loading, onShowMore }: { hasMore: boolean; loading: boolean; onShowMore: () => void }) {
  if (loading) return <ListRowSkeletons />;
  return hasMore ? <Button size="sm" variant="ghost" className="self-start" onClick={onShowMore}>Show more</Button> : null;
}
