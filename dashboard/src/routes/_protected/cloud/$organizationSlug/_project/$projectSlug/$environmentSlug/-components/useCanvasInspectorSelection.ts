import { useMatch } from "@tanstack/react-router";

const ENVIRONMENT_SERVICE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId";
const ENVIRONMENT_RESOURCE_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId";

export type CanvasInspectorSelection = {
  selectedServiceId: string | null;
  selectedResourceId: string | null;
  /** Id of whichever node's inspector is open, regardless of node type. */
  selectedNodeId: string | null;
  isInspectorOpen: boolean;
};

/**
 * Single source of truth for which canvas node's inspector is currently open.
 *
 * Every place that reacts to the inspector (layout, save bar, node centering)
 * should read from here instead of matching routes by hand, so adding a new
 * inspector route type only means editing this hook.
 */
export function useCanvasInspectorSelection(): CanvasInspectorSelection {
  const serviceMatch = useMatch({
    from: ENVIRONMENT_SERVICE_ROUTE_ID,
    shouldThrow: false,
  });
  const resourceMatch = useMatch({
    from: ENVIRONMENT_RESOURCE_ROUTE_ID,
    shouldThrow: false,
  });
  const selectedServiceId = serviceMatch?.params.serviceId ?? null;
  const selectedResourceId = resourceMatch?.params.resourceId ?? null;
  const selectedNodeId = selectedServiceId ?? selectedResourceId;

  return {
    selectedServiceId,
    selectedResourceId,
    selectedNodeId,
    isInspectorOpen: selectedNodeId != null,
  };
}
