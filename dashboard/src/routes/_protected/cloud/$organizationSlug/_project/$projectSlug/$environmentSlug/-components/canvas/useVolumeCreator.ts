import { useRef, useState } from "react";
import type { VolumeKind } from "@ployz/sdk";
import { useReactFlow } from "@xyflow/react";
import { useLoaderData } from "@tanstack/react-router";
import { usePlaceNewNode } from "./useCanvasPositionMutation";
import { createVolumeCommand } from "#/modules/config-store/store-volumes";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { slugifySegment } from "#/utils/slug";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { findPlacement } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-utils/node-placement";
import { SERVICE_NODE_SIZE } from "./constants";
import type { CanvasResourceNode, FlowPosition } from "./types";

export function useVolumeCreator(
  params: {
    organizationSlug: string;
    projectSlug: string;
    environmentSlug: string;
  },
  environmentId: string,
  getViewportCenter: () => FlowPosition,
) {
  const flow = useReactFlow<CanvasResourceNode>();
  const place = usePlaceNewNode(params.organizationSlug);
  const writer = useStoreWriter(params.organizationSlug);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [creatorOpen, setCreatorOpen] = useState(false);
  const [creatorPosition, setCreatorPosition] = useState<FlowPosition>({
    x: 0,
    y: 0,
  });
  const lastRightClickFlowPosition = useRef<FlowPosition>({ x: 0, y: 0 });

  function computePlacement(target: FlowPosition) {
    const existingRects = flow.getNodes().map((node) => ({
      x: node.position.x,
      y: node.position.y,
      ...SERVICE_NODE_SIZE,
    }));
    return findPlacement(target, SERVICE_NODE_SIZE, existingRects);
  }

  function openCreator(position: FlowPosition) {
    setCreatorPosition(computePlacement(position));
    setCreatorOpen(true);
  }

  function openCreatorAtCenter() {
    openCreator(getViewportCenter());
  }

  function openCreatorAtPosition(position: FlowPosition) {
    openCreator(position);
  }

  function openCreatorAtLastRightClick() {
    openCreator(lastRightClickFlowPosition.current);
  }

  function onPaneContextMenu(event: MouseEvent | React.MouseEvent) {
    lastRightClickFlowPosition.current = flow.screenToFlowPosition({
      x: event.clientX,
      y: event.clientY,
    });
  }

  /** On the canvas at once, saved in the background; a refused create goes again with a toast. */
  function createVolume(input: {
    name: string;
    storage: VolumeKind;
    position: FlowPosition;
  }) {
    const id = crypto.randomUUID();
    place({ environmentId, resourceType: "volume", resourceId: id, ...input.position });
    writer.commit(createVolumeCommand(id, store, slugifySegment(input.name) || "data", input.storage));
  }

  return {
    creatorOpen,
    setCreatorOpen,
    creatorPosition,
    openCreatorAtCenter,
    openCreatorAtPosition,
    openCreatorAtLastRightClick,
    onPaneContextMenu,
    createVolume,
  };
}
