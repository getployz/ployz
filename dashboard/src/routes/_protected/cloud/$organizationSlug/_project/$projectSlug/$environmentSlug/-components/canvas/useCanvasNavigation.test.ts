// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  blurClickedNodeLink,
  getNodePanDelta,
  shouldCenterSelectedNode,
  viewportShowing,
} from "./useCanvasNavigation";
import type { CanvasServiceNode } from "./types";

it("clears mouse focus without stealing keyboard focus", () => {
  const link = document.createElement("a");
  link.href = "#service";
  const title = document.createElement("span");
  link.append(title);
  document.body.append(link);
  link.addEventListener("click", (event) => {
    event.preventDefault();
    blurClickedNodeLink(event);
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

it("zooms out only as far as a box needs to fit beside the pane, then pans the least", () => {
  const start = { x: 0, y: 0, zoom: 1 };
  expect(viewportShowing(start, { x: 0, y: 0, width: 1000, height: 100 }, 548, 600, 0.4)).toEqual({ zoom: 0.5, x: 24, y: 24 });
  // Already visible: nothing moves.
  expect(viewportShowing(start, { x: 100, y: 100, width: 200, height: 100 }, 548, 600, 0.4)).toEqual(start);
  // Too big even at the minimum zoom: centred.
  expect(viewportShowing(start, { x: 0, y: 0, width: 5000, height: 100 }, 548, 600, 0.4)).toMatchObject({ zoom: 0.4, x: -726 });
});
