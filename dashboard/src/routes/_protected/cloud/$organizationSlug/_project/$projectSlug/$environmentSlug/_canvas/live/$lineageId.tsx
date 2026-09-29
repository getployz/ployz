import { createFileRoute } from "@tanstack/react-router";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { StoreLiveNodePanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreLiveNodePanel";

/** A Branch's Live Node, opened from the canvas by its name: whose it is and who here uses it. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/live/$lineageId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Live service" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreLiveNodePanel name={Route.useParams().lineageId} />;
}
