import { createFileRoute } from "@tanstack/react-router";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { VariableGroupDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VariableGroupDrawer";
import { useVariableGroupDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVariableGroupDrawerState";
import { VolumeDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeDrawer";
import { useVolumeDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVolumeDrawerState";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Resource" />,
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const volumeState = useVolumeDrawerState(params);
  const variableGroupState = useVariableGroupDrawerState(params);

  if (volumeState) {
    return <VolumeDrawer params={params} state={volumeState} />;
  }

  if (variableGroupState) {
    return <VariableGroupDrawer params={params} state={variableGroupState} />;
  }

  return null;
}
