import { RouteErrorAlert } from "#/components/route-error-alert";
import { Card, CardContent, CardHeader } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { useParams } from "@tanstack/react-router";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

export function CanvasInspectorPending() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return (
    <div className="flex h-full flex-col">
      <CanvasInspectorHeader params={params}>
        <Skeleton className="h-5 w-40 max-w-full" />
      </CanvasInspectorHeader>
      <div role="status" aria-label="Loading resource" className="flex flex-col gap-4 overflow-hidden p-4">
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
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return (
    <div className="flex h-full flex-col">
      <CanvasInspectorHeader params={params}>{noun}</CanvasInspectorHeader>
      <div className="p-4">
        <RouteErrorAlert
          title={`${noun} couldn’t load`}
          description={`The ${noun.toLowerCase()} details are unavailable right now. Try loading them again.`}
        />
      </div>
    </div>
  );
}
