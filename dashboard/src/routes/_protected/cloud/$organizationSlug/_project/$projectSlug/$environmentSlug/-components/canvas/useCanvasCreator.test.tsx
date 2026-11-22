// @vitest-environment jsdom

import { act, renderHook } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { useCanvasCreator } from "./useCanvasCreator";

// One Service node at the origin; the screen maps to the flow shifted by 100 on each axis.
vi.mock("@xyflow/react", () => ({
  useReactFlow: () => ({
    getNodes: () => [{ id: "web", position: { x: 0, y: 0 } }],
    screenToFlowPosition: ({ x, y }: { x: number; y: number }) => ({ x: x - 100, y: y - 100 }),
  }),
}));

const rightClick = (x: number, y: number) => ({ clientX: x, clientY: y }) as MouseEvent;

it("opens a creator where the pane was right-clicked, or in the view's middle from the toolbar", () => {
  const { result } = renderHook(() => useCanvasCreator(() => ({ x: 600, y: 600 })));
  act(() => result.current.onPaneContextMenu(rightClick(1100, 900)));
  act(() => result.current.openAtLastRightClick("volume"));
  expect(result.current.open).toBe("volume");
  expect(result.current.position).toEqual({ x: 1000, y: 800 });
  expect(result.current.placeAtLastRightClick()).toEqual({ x: 1000, y: 800 });

  act(() => result.current.openAtCenter("service", "database"));
  expect(result.current).toMatchObject({ open: "service", panel: "database", position: { x: 600, y: 600 } });
});

it("moves a node that would land on another clear of it, and a switch keeps that spot", () => {
  const { result } = renderHook(() => useCanvasCreator(() => ({ x: 0, y: 0 })));
  act(() => result.current.onPaneContextMenu(rightClick(110, 110)));
  act(() => result.current.openAtLastRightClick("service"));
  const landed = result.current.position;
  const clear = landed.x > 288 + 20 || landed.x + 288 + 20 < 0 || landed.y > 144 + 20 || landed.y + 144 + 20 < 0;
  expect(clear).toBe(true);
  expect(result.current.placeAtLastRightClick()).toEqual(landed);

  act(() => result.current.switchTo("config"));
  expect(result.current).toMatchObject({ open: "config", position: landed });
  act(() => result.current.setOpen("service", false));
  expect(result.current.open).toBe("config");
  act(() => result.current.setOpen("config", false));
  expect(result.current.open).toBeNull();
});
