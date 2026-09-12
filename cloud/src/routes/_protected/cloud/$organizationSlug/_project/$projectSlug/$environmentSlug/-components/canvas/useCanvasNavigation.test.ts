// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import { renderHook, cleanup } from "@testing-library/react";
import { ReactFlowProvider } from "@xyflow/react";
import {
  getCanvasInspectorOffsetX,
  shouldCenterSelectedNode,
  useCanvasNavigation,
} from "./useCanvasNavigation";
import type { CanvasServiceNode } from "./types";

it("clears mouse focus without stealing keyboard focus", () => {
  const { result } = renderHook(() => useCanvasNavigation(null, null, false), {
    wrapper: ReactFlowProvider,
  });
  const link = document.createElement("a");
  link.href = "#service";
  const title = document.createElement("span");
  link.append(title);
  document.body.append(link);
  link.addEventListener("click", (event) => {
    event.preventDefault();
    result.current.onNodeClick(event);
  });
  try {
    link.focus();
    title.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 }));
    expect(document.activeElement).not.toBe(link);

    link.focus();
    title.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 0 }));
    expect(document.activeElement).toBe(link);
  } finally {
    link.remove();
    cleanup();
  }
});

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
