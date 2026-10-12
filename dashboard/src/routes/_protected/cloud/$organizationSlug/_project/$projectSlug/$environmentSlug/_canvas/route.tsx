import { Suspense } from "react";
import { createFileRoute, type ErrorComponentProps } from "@tanstack/react-router";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { requireStoreEnvironment, prefetchRemoteUptime } from "#/collections/route-data";
import { environmentUptimeQueryOptions } from "#/modules/uptime/uptime.queries";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  // Open uptime incidents add to the canvas ⚠ counts. A failed read leaves them as they were.
  loader: async ({ params, context }) => {
    const environment = await requireStoreEnvironment(context, params);
    await prefetchRemoteUptime(context, params.organizationSlug, environmentUptimeQueryOptions({ organizationSlug: params.organizationSlug, environmentId: environment.id }));
  },
  errorComponent: CanvasError,
  component: CanvasLayout,
});

function CanvasLayout() {
  return (
    <div className="h-full overflow-hidden">
      <Suspense fallback={<PendingCanvas />}>
        <EnvironmentCanvasScene />
      </Suspense>
    </div>
  );
}

function CanvasError({ error }: ErrorComponentProps) {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <RouteErrorAlert
        title="Couldn’t load this environment"
        description={
          (error instanceof Error && error.message) ||
          "The environment data could not be loaded."
        }
      />
    </div>
  );
}
