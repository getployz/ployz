// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import { renderHook, cleanup } from "@testing-library/react";
import { ReactFlowProvider } from "@xyflow/react";
import {
  getNodePanDelta,
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

describe("getNodePanDelta", () => {
  it("keeps visible nodes still and moves obscured nodes only to the visible edge", () => {
    expect(getNodePanDelta(50, 200, 500)).toBe(0);
    expect(getNodePanDelta(-10, 200, 500)).toBe(34);
    expect(getNodePanDelta(400, 200, 500)).toBe(-124);
    expect(getNodePanDelta(100, 300, 250)).toBe(-125);
  });
});
