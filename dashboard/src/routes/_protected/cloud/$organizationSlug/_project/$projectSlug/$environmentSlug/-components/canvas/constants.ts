export const SERVICE_NODE_WIDTH = 288;
export const SERVICE_NODE_HEIGHT = 144;
export const CANVAS_MIN_ZOOM = 0.4;
export const CANVAS_MAX_ZOOM = 1.35;
export const SERVICE_NODE_SIZE = {
  width: SERVICE_NODE_WIDTH,
  height: SERVICE_NODE_HEIGHT,
};
export const SNAP_GRID: [number, number] = [24, 24];

/** Fitting the canvas keeps nodes clear of the controls over it: Create and Branch above, the bottom bar below. */
export const CANVAS_FIT_VIEW = { padding: { top: "72px", bottom: "96px", x: "48px" } } as const;
