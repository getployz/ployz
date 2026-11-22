import type { environmentCanvasNodePosition } from "./tables";
import { canvasPositionKey } from "./canvas-positions";
import { defineOrgCollection } from "#/collections/change-log/declare";

export type CanvasPositionRow = typeof environmentCanvasNodePosition.$inferSelect;

/** Where each Environment node sits on the canvas. */
export const canvasPositions = defineOrgCollection({
  name: "environment_canvas_node_position",
  sources: ["environment_canvas_node_position"],
  key: (row: CanvasPositionRow) => canvasPositionKey(row),
});
