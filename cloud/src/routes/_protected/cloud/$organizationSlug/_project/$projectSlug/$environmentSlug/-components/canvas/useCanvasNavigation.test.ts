import { describe, expect, it } from "vitest";
import {
  getCanvasInspectorOffsetX,
  shouldCenterSelectedNode,
} from "./useCanvasNavigation";
import type { CanvasServiceNode } from "./types";

function createNode(
  overrides?: Partial<CanvasServiceNode>,
): CanvasServiceNode {
  return {
    id: "service-1",
    type: "service",
    position: { x: 100, y: 200 },
    data: {
      resourceType: "service",
      resourceId: "service-1",
      serviceId: "service-1",
      environmentId: "env-1",
    },
    ...overrides,
  };
}

describe("shouldCenterSelectedNode", () => {
  it("recenters when the selected node position changes", () => {
    expect(
      shouldCenterSelectedNode({
        selectedNode: createNode({
          position: { x: 300, y: 400 },
        }),
        selectedNodeId: "service-1",
        previousSelectedNodeId: "service-1",
        previousSelectedNodePositionKey: "100:200",
      }),
    ).toBe(true);
  });

  it("does not recenter while the selected node is being dragged", () => {
    expect(
      shouldCenterSelectedNode({
        selectedNode: createNode({
          dragging: true,
          position: { x: 300, y: 400 },
        }),
        selectedNodeId: "service-1",
        previousSelectedNodeId: "service-1",
        previousSelectedNodePositionKey: "100:200",
      }),
    ).toBe(false);
  });
});

describe("getCanvasInspectorOffsetX", () => {
  it("uses the measured inspector pane width for the selected-node offset", () => {
    expect(
      getCanvasInspectorOffsetX({
        flowWidth: 1200,
        paneWidth: 600,
        zoom: 1.5,
      }),
    ).toBe(200);
  });

  it("does not offset when the service pane covers the flow", () => {
    expect(
      getCanvasInspectorOffsetX({
        flowWidth: 1000,
        paneWidth: 950,
        zoom: 1.5,
      }),
    ).toBeNull();
  });
});
