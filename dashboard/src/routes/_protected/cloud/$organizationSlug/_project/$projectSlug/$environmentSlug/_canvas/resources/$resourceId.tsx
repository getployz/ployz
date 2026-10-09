import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemote } from "#/collections/route-data";
import { volumeRunsQueryOptions } from "#/modules/volume-run/volume-run.queries";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { StoreVolumeDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/StoreVolumeDrawer";

// The Environment's loader prefetched its Volumes; a Volume's runs come here, for its Copies.
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId",
)({
  loader: ({ params, context }) => prefetchRemote(context, volumeRunsQueryOptions(params.organizationSlug, params.resourceId)),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Resource" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreVolumeDrawer params={Route.useParams()} />;
}
