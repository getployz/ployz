import { useCollectionScope } from "#/collections/use-collection-scope";
import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useLoaderData } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { getCanvasPositionsCollection } from "#/collections/collections";
import { updateCanvasPositionServerFn } from "#/modules/canvas/canvas-positions.functions";
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
  const collectionScope = useCollectionScope();
  const flow = useReactFlow<CanvasResourceNode>();
  const place = useServerFn(updateCanvasPositionServerFn);
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

  async function createVolume(input: {
    name: string;
    position: FlowPosition;
  }) {
    const id = crypto.randomUUID();
    // Placed first, so it appears where it was put rather than jumping there.
    const placed = await place({ data: { organizationSlug: params.organizationSlug, environmentId, resourceType: "volume", resourceId: id,
      x: Math.round(input.position.x), y: Math.round(input.position.y) } });
    await getCanvasPositionsCollection(params.organizationSlug, collectionScope).writeCommitted(placed.data);
    await writer.commit(createVolumeCommand(id, store, slugifySegment(input.name) || "data")).isPersisted.promise;
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
