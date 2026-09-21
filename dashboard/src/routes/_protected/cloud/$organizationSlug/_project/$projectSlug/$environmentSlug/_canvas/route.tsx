import { environmentManager, useSuspenseQuery } from "@tanstack/react-query";
import { Suspense } from "react";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { environmentCanvasOptions } from "#/modules/environment-design/environment-data";
import {
  createFileRoute,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  loader: async ({ params, context }) => {
    const scope = { environmentSlug: params.environmentSlug, queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    const options = environmentCanvasOptions(params, scope);
    if (environmentManager.isServer()) {
      await context.queryClient.ensureQueryData(options);
    } else {
      void context.queryClient.prefetchQuery(options);
    }
  },
  pendingComponent: CanvasPending,
  errorComponent: CanvasError,
  component: CanvasLayout,
});

function CanvasLayout() {
  return (
    <div className="h-full overflow-hidden">
      <Suspense fallback={<PendingCanvas />}>
        <CanvasContent />
      </Suspense>
    </div>
  );
}

function CanvasContent() {
  useSuspenseQuery(environmentCanvasOptions(Route.useParams(), useCollectionScope()));
  return <EnvironmentCanvasScene />;
}

function CanvasPending() {
  return (
    <div className="h-full overflow-hidden">
      <PendingCanvas />
    </div>
  );
}

function CanvasError({ error }: ErrorComponentProps) {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <RouteErrorAlert
        title="Couldn’t load this environment"
        description={
          error.message || "The environment data could not be loaded."
        }
      />
    </div>
  );
}
