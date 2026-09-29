import { applyCreatedService } from "#/modules/environment-design/apply-created-node";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useLoaderData, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import type { EnvironmentRef } from "@ployz/sdk";
import { getCanvasPositionsCollection } from "#/collections/collections";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { createServiceCommand, newServiceName, type NewServiceSource } from "#/modules/config-store/store-services";
import { servicesQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { createServiceServerFn, updateServiceCanvasPositionServerFn } from "#/modules/environment-design/service-functions";
import { createEmptyServiceSource } from "#/modules/environment-design/services";
import { SERVICE_NODE_SIZE } from "./constants";
import type { CanvasServiceNode, CreatorPanel, FlowPosition } from "./types";
import { findPlacement } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-utils/node-placement";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";

/**
 * Creates a Service in the Config Store where the canvas put it, named from its source. Resolves with its id once
 * every Store view shows it, so the page can open it.
 */
export function useCreateStoreService(organizationSlug: string) {
  const scope = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  const placeService = useServerFn(updateServiceCanvasPositionServerFn);
  return async (target: { store: EnvironmentRef; environmentId: string; position: FlowPosition }, source: NewServiceSource) => {
    const listed = scope.queryClient.getQueryData(storeViewOptions(organizationSlug, scope, servicesQuery(target.store)).queryKey);
    const id = crypto.randomUUID();
    // Placed first, so it appears where it was put rather than jumping there.
    const placed = await placeService({ data: { organizationSlug, environmentId: target.environmentId, serviceId: id,
      x: Math.round(target.position.x), y: Math.round(target.position.y) } });
    await getCanvasPositionsCollection(organizationSlug, scope).writeCommitted(placed.data);
    const name = newServiceName(source, listed?.ok ? listed.value.services : []);
    await writer.commit(createServiceCommand(id, target.store, name, source)).isPersisted.promise;
    return { service: { id } };
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
  const collectionScope = useCollectionScope();
  const flow = useReactFlow<CanvasServiceNode>();
  const createService = useServerFn(createServiceServerFn);
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
    if (storeEnabled) {
      const created = await createStoreService({ store, environmentId, position: placement }, { type: "empty" });
      await navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: created.service.id } });
      return;
    }
    const receipt = await createService({
      data: {
        organizationSlug: params.organizationSlug,
        environmentId,
        source: createEmptyServiceSource(),
        x: placement.x,
        y: placement.y,
      },
    });
    await applyCreatedService(params.organizationSlug, collectionScope, receipt.data);
    await navigate({
      to: ENVIRONMENT_SERVICE_ROUTE_TO,
      params: {
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug: params.environmentSlug,
        serviceId: receipt.data.service.id,
      },
    });
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
