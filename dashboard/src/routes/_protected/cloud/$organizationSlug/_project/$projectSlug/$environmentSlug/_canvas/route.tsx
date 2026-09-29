import { Suspense } from "react";
import {
  createFileRoute,
  redirect,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { prefetchActiveBuildTails, prefetchFromOrgStore } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";
import { DEPLOYMENT_LIST_ROUTE_TO, DEPLOYMENT_PAGE_ROUTE_TO, legacyDeploymentLink } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-page";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  // Old links: Deployment Mode's `?deployment=<id>` (on the canvas or a service) opens that attempt's Deployment Page, and
  // the old deploy bar's `?deploymentList=true` opens the deployment list.
  beforeLoad: ({ location, params }) => {
    const legacy = legacyDeploymentLink(location);
    if (legacy) throw redirect({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId: legacy.deploymentId }, search: legacy.search, replace: true });
    if (new URLSearchParams(location.searchStr).get("deploymentList") === "true") throw redirect({ to: DEPLOYMENT_LIST_ROUTE_TO, params, replace: true });
  },
  // The canvas and every panel over it (the deployment list among them) show the bottom bar, whose running attempt, like
  // the list's active rows, reads its build tail.
  // Over the Config Store the Environment's loader prefetched its Deployments instead.
  loader: ({ params, context }) => storeEnabled ? undefined
    : prefetchFromOrgStore(context, params.organizationSlug, () => [prefetchActiveBuildTails(context, params)]),
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
          error.message || "The environment data could not be loaded."
        }
      />
    </div>
  );
}
