import { createFileRoute } from "@tanstack/react-router";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { LiveNodePanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/LiveNodePanel";
import { StoreLiveNodePanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreLiveNodePanel";
import { storeEnabled } from "#/modules/config-store/store.contract";

/** A Branch's Live Node, opened from the canvas: whose it is and who here uses it. Over the Store, the param is its name. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/live/$lineageId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Live service" />,
  component: RouteComponent,
});

function RouteComponent() {
  const { lineageId } = Route.useParams();
  return storeEnabled ? <StoreLiveNodePanel name={lineageId} /> : <LiveNodePanel lineageId={lineageId} />;
}
