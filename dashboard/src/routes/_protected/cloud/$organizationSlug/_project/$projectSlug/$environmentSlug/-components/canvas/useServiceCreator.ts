import { useCollectionScope } from "#/collections/use-collection-scope";
import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useLoaderData, useNavigate } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
import { createServiceCommand, newServiceName, type NewServiceSource } from "#/modules/config-store/store-services";
import { servicesQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { usePlaceNewNode } from "./useCanvasPositionMutation";
import { SERVICE_NODE_SIZE } from "./constants";
import type { CanvasResourceNode, CreatorPanel, FlowPosition } from "./types";
import { findPlacement } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-utils/node-placement";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";

/**
 * Creates a Service in the Config Store where the canvas put it, named from its source: on the canvas at once, saved
 * in the background. `persisted` settles once the Store has it, for callers whose next page can't show it before.
 */
export function useCreateStoreService(organizationSlug: string) {
  const scope = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  const place = usePlaceNewNode(organizationSlug);
  return (target: { store: EnvironmentRef; environmentId: string; position: FlowPosition }, source: NewServiceSource) => {
    const listed = scope.queryClient.getQueryData(storeViewOptions(organizationSlug, scope, servicesQuery(target.store)).queryKey);
    const id = crypto.randomUUID();
    place({ environmentId: target.environmentId, resourceType: "service", resourceId: id, ...target.position });
    const name = newServiceName(source, listed?.ok ? listed.value.services : []);
    const { isPersisted } = writer.commit(createServiceCommand(id, target.store, name, source));
    return { service: { id }, persisted: isPersisted.promise };
  };
}

export function useServiceCreator(
  params: {
    organizationSlug: string;
    projectSlug: string;
    environmentSlug: string;
  },
  environmentId: string,
  getViewportCenter: () => FlowPosition,
) {
  const navigate = useNavigate();
  const flow = useReactFlow<CanvasResourceNode>();
  const createStoreService = useCreateStoreService(params.organizationSlug);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });

  const [creatorOpen, setCreatorOpen] = useState(false);
  const [creatorPosition, setCreatorPosition] = useState<FlowPosition>({
    x: 0,
    y: 0,
  });
  const [creatorPanel, setCreatorPanel] = useState<CreatorPanel>("root");
  const lastRightClickFlowPosition = useRef<FlowPosition>({ x: 0, y: 0 });

  function computePlacement(target: FlowPosition) {
    const existingRects = flow.getNodes().map((node) => ({
      x: node.position.x,
      y: node.position.y,
      ...SERVICE_NODE_SIZE,
    }));
    return findPlacement(target, SERVICE_NODE_SIZE, existingRects);
  }

  function openCreator(position: FlowPosition, panel: CreatorPanel = "root") {
    setCreatorPosition(computePlacement(position));
    setCreatorPanel(panel);
    setCreatorOpen(true);
  }

  function openCreatorAtCenter(panel: CreatorPanel = "root") {
    openCreator(getViewportCenter(), panel);
  }

  async function createBlankService(position: FlowPosition) {
    const placement = computePlacement(position);
    const created = createStoreService({ store, environmentId, position: placement }, { type: "empty" });
    await navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: created.service.id } });
  }

  function onPaneContextMenu(event: MouseEvent | React.MouseEvent) {
    lastRightClickFlowPosition.current = flow.screenToFlowPosition({
      x: event.clientX,
      y: event.clientY,
    });
  }

  function openCreatorAtLastRightClick(panel: CreatorPanel) {
    openCreator(lastRightClickFlowPosition.current, panel);
  }

  function createBlankServiceAtLastRightClick() {
    void createBlankService(lastRightClickFlowPosition.current).catch((error) =>
      toast.error(toErrorMessage(error, "The service couldn’t be created. Try again.")),
    );
  }

  return {
    creatorOpen,
    setCreatorOpen,
    creatorPosition,
    creatorPanel,
    openCreatorAtCenter,
    onPaneContextMenu,
    openCreatorAtLastRightClick,
    createBlankServiceAtLastRightClick,
  };
}
