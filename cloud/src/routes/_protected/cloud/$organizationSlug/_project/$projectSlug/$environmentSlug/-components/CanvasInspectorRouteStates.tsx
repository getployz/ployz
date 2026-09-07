import { RouteErrorAlert } from "#/components/route-error-alert";
import { Card, CardContent, CardHeader } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";

export function CanvasInspectorPending() {
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b p-4">
        <Skeleton className="size-9 rounded-full" />
        <div className="flex flex-1 flex-col gap-2">
          <Skeleton className="h-5 w-40" />
          <Skeleton className="h-3 w-24" />
        </div>
        <Skeleton className="size-9" />
      </div>
      <div className="flex flex-col gap-4 overflow-hidden p-4">
        <Skeleton className="h-9 w-full" />
        {Array.from({ length: 3 }, (_, index) => (
          <Card key={index}>
            <CardHeader>
              <Skeleton className="h-5 w-32" />
            </CardHeader>
            <CardContent className="flex flex-col gap-3">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-9 w-full" />
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  );
}

export function CanvasInspectorError({ noun }: { noun: string }) {
  return (
    <div className="p-4">
      <RouteErrorAlert
        title={`${noun} couldn’t load`}
        description={`The ${noun.toLowerCase()} details are unavailable right now. Try loading them again.`}
      />
    </div>
  );
}
