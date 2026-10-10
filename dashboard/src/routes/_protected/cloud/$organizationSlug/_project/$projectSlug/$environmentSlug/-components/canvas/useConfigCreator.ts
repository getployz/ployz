import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useLoaderData, useNavigate } from "@tanstack/react-router";
import { usePlaceNewNode } from "./useCanvasPositionMutation";
import { createConfigCommand } from "#/modules/config-store/store-configs";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { slugifySegment } from "#/utils/slug";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, type EnvironmentRouteParams } from "../environment-route-paths";
import { findPlacement } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-utils/node-placement";
import { SERVICE_NODE_SIZE } from "./constants";
import type { CanvasResourceNode, FlowPosition } from "./types";

/** Creates a Config where the user asked, then opens it so its first file can be added. */
export function useConfigCreator(params: EnvironmentRouteParams, environmentId: string, getViewportCenter: () => FlowPosition) {
  const flow = useReactFlow<CanvasResourceNode>();
  const place = usePlaceNewNode(params.organizationSlug);
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [creatorOpen, setCreatorOpen] = useState(false);
  const [creatorPosition, setCreatorPosition] = useState<FlowPosition>({ x: 0, y: 0 });
  const lastRightClick = useRef<FlowPosition>({ x: 0, y: 0 });

  function openCreatorAt(target: FlowPosition) {
    const taken = flow.getNodes().map((node) => ({ ...node.position, ...SERVICE_NODE_SIZE }));
    setCreatorPosition(findPlacement(target, SERVICE_NODE_SIZE, taken));
    setCreatorOpen(true);
  }

  async function createConfig(name: string) {
    const id = crypto.randomUUID();
    place({ environmentId, resourceType: "config", resourceId: id, ...creatorPosition });
    await writer.commit(createConfigCommand(id, store, slugifySegment(name) || "config")).isPersisted.promise;
    await navigate({ to: ENVIRONMENT_RESOURCE_ROUTE_TO, params: { ...params, resourceId: id }, search: (prev) => prev });
  }

  return {
    creatorOpen,
    setCreatorOpen,
    openCreatorAtCenter: () => openCreatorAt(getViewportCenter()),
    openCreatorAtPosition: openCreatorAt,
    openCreatorAtLastRightClick: () => openCreatorAt(lastRightClick.current),
    onPaneContextMenu: (event: MouseEvent | React.MouseEvent) => {
      lastRightClick.current = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
    },
    createConfig,
  };
}
