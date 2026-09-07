import { DashboardPage } from "#/components/dashboard-page";
import {
  Card,
  CardAction,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";

export function ServersSkeleton({ listOnly = false }: { listOnly?: boolean }) {
  const rows = (
    <div className="flex flex-col gap-3" role="status" aria-label="Servers loading">
      {[0, 1, 2].map((row) => (
        <Card key={row} size="sm">
          <CardHeader>
            <CardTitle>
              <Skeleton className="h-4 w-32" />
            </CardTitle>
            <CardDescription>
              <Skeleton className="h-3 w-64 max-w-full" />
            </CardDescription>
            <CardAction>
              <Skeleton className="h-5 w-20" />
            </CardAction>
          </CardHeader>
        </Card>
      ))}
    </div>
  );

  if (listOnly) return rows;

  return (
    <DashboardPage>
      <div className="flex items-center gap-3">
        <Skeleton className="h-9 flex-1" />
        <Skeleton className="h-9 w-32" />
      </div>
      {rows}
    </DashboardPage>
  );
}
