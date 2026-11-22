import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import type { CreatePanel } from "#/routes/_protected/cloud/$organizationSlug/_project/-components/service-create/create-menu-items";
import { findPlacement } from "../../-utils/node-placement";
import { SERVICE_NODE_SIZE } from "./constants";
import type { CanvasResourceNode, FlowPosition } from "./types";

export type CreatorKind = "service" | "volume" | "config";

/**
 * Which creator dialog is open and where its new node lands: the nearest free spot to the last right-click on the
 * pane, or to the middle of the view for the toolbar's Create.
 */
export function useCanvasCreator(getViewportCenter: () => FlowPosition) {
  const flow = useReactFlow<CanvasResourceNode>();
  const [open, setOpen] = useState<CreatorKind | null>(null);
  const [position, setPosition] = useState<FlowPosition>({ x: 0, y: 0 });
  const [panel, setPanel] = useState<CreatePanel>("root");
  const lastRightClick = useRef<FlowPosition>({ x: 0, y: 0 });

  function placeNear(target: FlowPosition) {
    const taken = flow.getNodes().map((node) => ({ ...node.position, ...SERVICE_NODE_SIZE }));
    return findPlacement(target, SERVICE_NODE_SIZE, taken);
  }

  function openAt(kind: CreatorKind, target: FlowPosition, servicePanel: CreatePanel = "root") {
    setPosition(placeNear(target));
    if (kind === "service") setPanel(servicePanel);
    setOpen(kind);
  }

  return {
    open,
    position,
    panel,
    /** Closing one dialog leaves another that opened in the meantime open. */
    setOpen: (kind: CreatorKind, next: boolean) => setOpen((current) => (next ? kind : current === kind ? null : current)),
    openAtCenter: (kind: CreatorKind, servicePanel?: CreatePanel) => openAt(kind, getViewportCenter(), servicePanel),
    openAtLastRightClick: (kind: CreatorKind, servicePanel?: CreatePanel) => openAt(kind, lastRightClick.current, servicePanel),
    /** Moves to another creator, landing its node where the open one's would have. */
    switchTo: (kind: CreatorKind) => openAt(kind, position),
    placeAtLastRightClick: () => placeNear(lastRightClick.current),
    onPaneContextMenu: (event: MouseEvent | React.MouseEvent) => {
      lastRightClick.current = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
    },
  };
}
