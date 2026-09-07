import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useServerFn } from "@tanstack/react-start";
import { getRawEnvironmentResourcesCollection } from "#/electric/collections";
import { createVolumeResourceServerFn } from "#/modules/environment-design/resource-functions";
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
  const createVolumeResource = useServerFn(createVolumeResourceServerFn);
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

  async function createVolume(input: {
    name: string;
    position: FlowPosition;
  }) {
    const result = await createVolumeResource({
      data: {
        organizationSlug: params.organizationSlug,
        environmentId,
        name: input.name,
        x: input.position.x,
        y: input.position.y,
      },
    });

    await getRawEnvironmentResourcesCollection(
      params.organizationSlug,
    ).utils.awaitTxId(result.txid);

    return result.data;
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
