import { prefetchRemote, requireEnvironment } from "#/collections/route-data";
import { latestVolumeRemoveAttemptQueryOptions } from "#/modules/runtime/volume-removal.queries";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "../../-components/environment-route-paths";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { VolumeDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeDrawer";
import { useVolumeDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVolumeDrawerState";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId",
)({
  loader: async ({ params, context }) => {
    const environment = await requireEnvironment(context, params);
    await prefetchRemote(context, latestVolumeRemoveAttemptQueryOptions({
      organizationSlug: params.organizationSlug, environmentId: environment.id, resourceId: params.resourceId,
    }));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Resource" />,
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const volumeState = useVolumeDrawerState(params);

  if (volumeState) {
    return <VolumeDrawer params={params} state={volumeState} />;
  }

  throw redirect({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: {}, replace: true });
}
