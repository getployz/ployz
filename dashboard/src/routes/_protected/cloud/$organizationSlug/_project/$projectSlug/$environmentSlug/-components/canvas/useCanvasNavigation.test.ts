// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  blurClickedNodeLink,
  getNodePanDelta,
  viewportAcrossPages,
  viewportShowing,
} from "./useCanvasNavigation";

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
