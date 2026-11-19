import { Suspense } from "react";
import { createFileRoute, type ErrorComponentProps } from "@tanstack/react-router";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { prefetchStoreEnvironment, requireStoreEnvironment, streamRemote, streamRemoteUptime, streamStoreViews } from "#/collections/route-data";
import { configsQuery, diffQuery, domainsQuery, environmentSettingsQuery, namespaceQuery, servicesQuery, volumesQuery } from "#/modules/config-store/store-view.queries";
import { githubWebQueryOptions } from "#/modules/github/github.queries";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { environmentUptimeQueryOptions } from "#/modules/uptime/uptime.queries";
import { toErrorMessage } from "#/lib/error-message";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  loader: async ({ params, context }) => {
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    const environment = await requireStoreEnvironment(context, params);
    streamStoreViews(context, params.organizationSlug, domainsQuery(store));
    // Only the Sealed controls read it, and they show as off until it lands.
    streamRemote(context, sealingQueryOptions(params.organizationSlug));
    // Open uptime incidents add to the canvas ⚠ counts. A failed read leaves them as they were.
    streamRemoteUptime(context, params.organizationSlug, environmentUptimeQueryOptions({ organizationSlug: params.organizationSlug, environmentId: environment.id }));
    // GitHub's origin links a Git Service's repository and a merged pull request.
    await prefetchStoreEnvironment(context, params.organizationSlug, store,
      [environmentSettingsQuery(store), servicesQuery(store), diffQuery(store), volumesQuery(store), configsQuery(store), namespaceQuery(store)],
      githubWebQueryOptions());
  },
  errorComponent: CanvasError,
  component: CanvasLayout,
});

function CanvasLayout() {
  const { environmentSlug } = Route.useParams();
  return (
    <div className="h-full overflow-hidden">
      {/* The canvas fills the page and the top bar's crumbs name it, so its h1 is for screen readers. */}
      <h1 className="sr-only">{environmentSlug}</h1>
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
        description={toErrorMessage(error, "The environment data could not be loaded.")}
      />
    </div>
  );
}
