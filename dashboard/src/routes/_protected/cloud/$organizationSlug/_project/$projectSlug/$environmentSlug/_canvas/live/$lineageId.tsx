import { createFileRoute } from "@tanstack/react-router";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { LiveNodePanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/LiveNodePanel";

/** A Branch's Live Node, opened from the canvas: whose it is and who here uses it. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/live/$lineageId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Live service" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <LiveNodePanel lineageId={Route.useParams().lineageId} />;
}
