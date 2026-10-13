import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import type { VolumeCopies } from "#/modules/volume-copies/volume-copies";
import type { CanvasResourceNode } from "./types";
import {
  fitViewport,
  getNodePanDelta,
  nodesBox,
  viewportAcrossPages,
  viewportShowing,
} from "./useCanvasNavigation";

it("saves the viewport a Deployment Page opens over and starts from it again once the page closes", () => {
  const before = { x: 0, y: 0, zoom: 1 };
  const revealed = { x: -300, y: 20, zoom: 0.6 };
  const opened = viewportAcrossPages(null, true, before);
  expect(opened).toEqual({ start: null, saved: before });
  // Revealing (or the user panning) while the page stays open keeps the first saved viewport.
  expect(viewportAcrossPages(opened.saved, true, revealed)).toEqual({ start: null, saved: before });
  expect(viewportAcrossPages(opened.saved, false, revealed)).toEqual({ start: before, saved: null });
  expect(viewportAcrossPages(null, false, revealed)).toEqual({ start: null, saved: null });
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

it("widens a node's box by the copies hung under it, so revealing it keeps every card in view", () => {
  const server = { id: "web-2" as MachineId, name: "web-2" };
  const copies: VolumeCopies = {
    volume: { id: "v", name: "v" }, live: { server, restore: null, moveTo: null, busy: null, snapshots: null },
    elsewhere: [1, 2, 3].map((n) => ({ kind: "mirror", id: `m${n}`, server, name: `m${n}`, refreshedAt: null, skipped: null, staged: null, busy: null })),
  };
  const node = (hang: readonly VolumeCopies[]) =>
    asTestDouble<CanvasResourceNode>()({ position: { x: 100, y: 50 }, measured: { width: 288, height: 300 }, data: { hang } });
  expect(nodesBox([node([])])).toEqual({ x: 100, y: 50, width: 288, height: 300 });
  expect(nodesBox([node([copies])])).toEqual({ x: 100 - 132, y: 50, width: 288 + 2 * 132, height: 300 });
});

it("fits the whole canvas with the copies hung under a node in view, not just its card", () => {
  const server = { id: "web-2" as MachineId, name: "web-2" };
  const copies: VolumeCopies = {
    volume: { id: "v", name: "v" }, live: { server, restore: null, moveTo: null, busy: null, snapshots: null },
    elsewhere: [1, 2, 3].map((n) => ({ kind: "mirror", id: `m${n}`, server, name: `m${n}`, refreshedAt: null, skipped: null, staged: null, busy: null })),
  };
  const hung = asTestDouble<CanvasResourceNode>()({ position: { x: 0, y: 0 }, measured: { width: 288, height: 300 }, data: { hang: [copies] } });
  const viewport = fitViewport([hung], 400, 600);
  // The canvas's 48px side padding stays clear on both sides of the widest thing shown, the hung row.
  expect(-132 * viewport.zoom + viewport.x).toBeCloseTo(48);
  expect((288 + 132) * viewport.zoom + viewport.x).toBeCloseTo(400 - 48);
});
