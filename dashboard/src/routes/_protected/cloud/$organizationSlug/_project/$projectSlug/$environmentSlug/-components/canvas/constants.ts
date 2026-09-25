import type { CSSProperties } from "react";

/**
 * Names a node's element for the Live ↔ Deployment Mode view transition (styles.css), so each card moves in place across the switch.
 * ponytail: two canvases morphed by view transitions; one shared canvas instance fed mode-specific nodes is the upgrade path.
 */
export const canvasNodeTransition = (nodeId: string) => ({
  "data-canvas-transition": "",
  // SAFETY: React passes custom properties through; its CSSProperties type does not list them.
  style: { "--canvas-node": `canvas-node-${nodeId}` } as CSSProperties,
});

export const SERVICE_NODE_WIDTH = 288;
export const SERVICE_NODE_HEIGHT = 144;
export const SELECTED_SERVICE_ZOOM = 1.5;
export const SERVICE_NODE_SIZE = {
  width: SERVICE_NODE_WIDTH,
  height: SERVICE_NODE_HEIGHT,
};
export const SNAP_GRID: [number, number] = [24, 24];
