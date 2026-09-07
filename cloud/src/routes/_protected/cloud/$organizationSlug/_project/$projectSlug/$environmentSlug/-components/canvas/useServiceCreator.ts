import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { getRawServicesCollection } from "#/electric/collections";
import { createServiceServerFn } from "#/modules/environment-design/service-functions";
import { createEmptyServiceSource } from "#/modules/environment-design/services";
import { SERVICE_NODE_SIZE } from "./constants";
import type { CanvasServiceNode, CreatorPanel, FlowPosition } from "./types";
import { findPlacement } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-utils/node-placement";
import { ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";

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
  const flow = useReactFlow<CanvasServiceNode>();
  const createService = useServerFn(createServiceServerFn);

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
    const receipt = await createService({
      data: {
        organizationSlug: params.organizationSlug,
        environmentId,
        source: createEmptyServiceSource(),
        x: placement.x,
        y: placement.y,
      },
    });
    await getRawServicesCollection(
      params.organizationSlug,
    ).utils.awaitTxId(receipt.txid);
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
    void createBlankService(lastRightClickFlowPosition.current);
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
