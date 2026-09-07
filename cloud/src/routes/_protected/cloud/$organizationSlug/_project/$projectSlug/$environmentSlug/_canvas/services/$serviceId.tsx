import { createFileRoute } from "@tanstack/react-router";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { ServiceDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceDrawer";
import { useServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Service" />,
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const state = useServiceDrawerState(params);

  if (state == null) {
    return null;
  }

  return <ServiceDrawer params={params} state={state} />;
}
