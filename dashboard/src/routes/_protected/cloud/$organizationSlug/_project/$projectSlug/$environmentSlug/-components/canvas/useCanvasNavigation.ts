import { useEffect, useRef, useState } from "react";
import {
  getViewportForBounds,
  useNodesInitialized,
  useReactFlow,
} from "@xyflow/react";
import {
  CANVAS_FIT_VIEW,
  CANVAS_MAX_ZOOM,
  CANVAS_MIN_ZOOM,
  SERVICE_NODE_WIDTH,
  SERVICE_NODE_HEIGHT,
} from "./constants";
import type { CanvasResourceNode, FlowPosition } from "./types";

import { prefersReducedMotion } from "#/lib/motion";

const UNSET = Symbol("canvas-nav-unset");
const CANVAS_INSPECTOR_PANE_SELECTOR = "[data-canvas-inspector-pane]";
const CANVAS_INSPECTOR_FULL_WIDTH_RATIO = 0.9;

function getCanvasInspectorGeometryKey() {
  const wrapper = document.querySelector<HTMLElement>(".react-flow");
  const inspectorPane = document.querySelector<HTMLElement>(
    CANVAS_INSPECTOR_PANE_SELECTOR,
  );
  const flowWidth = Math.round(
    wrapper?.getBoundingClientRect().width ?? window.innerWidth,
  );
  const paneWidth = Math.round(
    inspectorPane?.getBoundingClientRect().width ?? 0,
  );
  const controls = wrapper?.closest(".environment-canvas-scene")
    ?.querySelector<HTMLElement>(".bottom-bar");

  return `${flowWidth}:${paneWidth}:${wrapper?.clientHeight ?? 0}:${controls?.offsetHeight ?? 0}`;
}

function useCanvasInspectorGeometryVersion(enabled: boolean) {
  const [version, setVersion] = useState(0);

  useEffect(() => {
    if (!enabled) {
      return;
    }

    let previousGeometryKey = getCanvasInspectorGeometryKey();
    let frameId: number | null = null;

    function notifyGeometryChanged() {
      if (frameId != null) {
        return;
      }

      frameId = window.requestAnimationFrame(() => {
        frameId = null;
        const nextGeometryKey = getCanvasInspectorGeometryKey();

        if (nextGeometryKey === previousGeometryKey) {
          return;
        }

        previousGeometryKey = nextGeometryKey;
        setVersion((currentVersion) => currentVersion + 1);
      });
    }

    const observer =
      "ResizeObserver" in globalThis
        ? new ResizeObserver(notifyGeometryChanged)
        : null;
    const wrapper = document.querySelector<HTMLElement>(".react-flow");
    const inspectorPane = document.querySelector<HTMLElement>(
      CANVAS_INSPECTOR_PANE_SELECTOR,
    );

    if (wrapper) {
      observer?.observe(wrapper);
    }

    if (inspectorPane) {
      observer?.observe(inspectorPane);
    }

    window.addEventListener("resize", notifyGeometryChanged);

    return () => {
      if (frameId != null) {
        window.cancelAnimationFrame(frameId);
      }

      observer?.disconnect();
      window.removeEventListener("resize", notifyGeometryChanged);
    };
  }, [enabled]);

  return version;
}

export function getNodePanDelta(start: number, size: number, available: number) {
  const margin = 24;
  if (size > available - margin * 2) {
    return (available - size) / 2 - start;
  }
  return Math.max(margin - start, Math.min(0, available - margin - start - size));
}

type Viewport = { x: number; y: number; zoom: number };
type Box = { x: number; y: number; width: number; height: number };

/**
 * The viewport that brings a box of flow coordinates into the visible `width` × `height`: zoomed out only as far as the
 * box needs (never below `minZoom`, never in), then panned the least that shows it.
 */
export function viewportShowing(viewport: Viewport, box: Box, width: number, height: number, minZoom: number): Viewport {
  const margin = 24 * 2;
  const zoom = Math.max(minZoom, Math.min(viewport.zoom, (width - margin) / box.width, (height - margin) / box.height));
  return {
    zoom,
    x: viewport.x + getNodePanDelta(box.x * zoom + viewport.x, box.width * zoom, width),
    y: viewport.y + getNodePanDelta(box.y * zoom + viewport.y, box.height * zoom, height),
  };
}

/**
 * The viewport, from `from`, that brings nodes into the canvas area the inspector pane leaves visible; `from` itself when
 * there are none. Null while no pane leaves room for them.
 */
function revealNodes(nodes: readonly CanvasResourceNode[], from: Viewport): Viewport | null {
  if (!nodes.length) return from;
  const wrapper = document.querySelector<HTMLElement>(".react-flow");
  const pane = document.querySelector<HTMLElement>(CANVAS_INSPECTOR_PANE_SELECTOR);
  if (!wrapper || !pane || pane.dataset["takeover"] === "true") return null;
  if (pane.offsetWidth / wrapper.clientWidth >= CANVAS_INSPECTOR_FULL_WIDTH_RATIO) {
    return null;
  }
  const controls = wrapper.closest(".environment-canvas-scene")
    ?.querySelector<HTMLElement>(".bottom-bar");
  const x = Math.min(...nodes.map((node) => node.position.x));
  const y = Math.min(...nodes.map((node) => node.position.y));
  const box = {
    x, y,
    width: Math.max(...nodes.map((node) => node.position.x + (node.measured?.width ?? SERVICE_NODE_WIDTH))) - x,
    height: Math.max(...nodes.map((node) => node.position.y + (node.measured?.height ?? SERVICE_NODE_HEIGHT))) - y,
  };
  return viewportShowing(from, box, wrapper.clientWidth - pane.offsetWidth,
    wrapper.clientHeight - (controls?.offsetHeight ?? 0), CANVAS_MIN_ZOOM);
}

/**
 * A Deployment Page saves the viewport it opened over; once it closes, the canvas starts again from that viewport.
 * `start`: where to start from (null: where the canvas is now). `saved`: what stays saved.
 */
export function viewportAcrossPages(saved: Viewport | null, pageOpen: boolean, current: Viewport) {
  return pageOpen ? { start: null, saved: saved ?? current } : { start: saved, saved: null };
}

/**
 * Keeps the canvas focus in view beside the inspector pane: the node that shows the selection (`focusNodeId`, at
 * `focusPositionKey`), else the nodes an open Deployment Page lights. Each is brought into view when it opens, when the
 * node moves, and when the pane resizes; closing a node's drawer fits the whole canvas again.
 */
export function useCanvasNavigation(
  focusNodeId: string | null,
  focusPositionKey: string | null,
  flowReady: boolean,
  /**
   * The open Deployment Page (`key`) and the nodes it lights, or the New branch panel and every node; null while neither
   * is open or the attempt loads.
   */
  deployment: { key: string; nodeIds: readonly string[] } | null,
) {
  const flow = useReactFlow<CanvasResourceNode>();
  const nodesInitialized = useNodesInitialized();
  const pageOpen = !focusNodeId && deployment !== null;
  const focusKey = focusNodeId ?? (deployment ? `deployment:${deployment.key}` : null);
  const geometryVersion = useCanvasInspectorGeometryVersion(flowReady && focusKey !== null);
  const previousFocusNodeId = useRef<string | null | symbol>(UNSET);
  const shown = useRef<{ focusKey: string | null; positionKey: string | null; geometryVersion: number } | null>(null);
  const savedViewport = useRef<Viewport | null>(null);
  // The ids live in a ref so the effect below reruns on the focus changing, not on each render's new array.
  const focusNodeIds = useRef<readonly string[]>([]);
  useEffect(() => {
    focusNodeIds.current = focusNodeId ? [focusNodeId] : deployment?.nodeIds ?? [];
  });

  useEffect(() => {
    if (!flowReady) {
      return;
    }

    // A closed drawer gives the canvas back whole: undo the pan that opening it caused, and show every node again.
    // A Deployment Page or the New branch panel opening instead keeps its own focus.
    if (focusNodeId === null && !pageOpen && previousFocusNodeId.current !== UNSET && previousFocusNodeId.current !== null) {
      // Not `fitView`: over controlled nodes it waits for a node change that may never come.
      const wrapper = document.querySelector<HTMLElement>(".react-flow");
      const nodes = flow.getNodes();
      if (wrapper && nodes.length) {
        void flow.setViewport(getViewportForBounds(flow.getNodesBounds(nodes), wrapper.clientWidth, wrapper.clientHeight,
          CANVAS_MIN_ZOOM, CANVAS_MAX_ZOOM, CANVAS_FIT_VIEW.padding), { duration: prefersReducedMotion() ? 0 : 360 });
      }
    }
    previousFocusNodeId.current = focusNodeId;
  }, [flow, flowReady, focusNodeId, pageOpen]);

  useEffect(() => {
    if (!flowReady) {
      return;
    }
    const across = viewportAcrossPages(savedViewport.current, pageOpen, flow.getViewport());
    savedViewport.current = across.saved;
    const start = across.start;
    const positionKey = focusNodeId ? focusPositionKey : null;
    const previous = shown.current;
    const changed = start !== null || previous?.focusKey !== focusKey || previous.positionKey !== positionKey
      || previous.geometryVersion !== geometryVersion;
    const nodes = () => focusNodeIds.current.flatMap((id) => flow.getNode(id) ?? []);
    if (!changed || nodes().some((node) => node.dragging)) {
      return;
    }

    function show() {
      const next = revealNodes(nodes(), start ?? flow.getViewport());
      const target = next ?? start;
      const current = flow.getViewport();
      if (target && (target.x !== current.x || target.y !== current.y || target.zoom !== current.zoom)) {
        void flow.setViewport(target, { duration: prefersReducedMotion() ? 0 : 360 });
      }
      if (next) shown.current = { focusKey, positionKey, geometryVersion };
      return next !== null;
    }

    if (show()) {
      return;
    }
    // The pane mounts with its route; measure again once it has laid out.
    let frameId = window.requestAnimationFrame(() => {
      frameId = window.requestAnimationFrame(show);
    });
    return () => window.cancelAnimationFrame(frameId);
  }, [flow, flowReady, nodesInitialized, focusKey, pageOpen, focusNodeId, focusPositionKey, geometryVersion]);

  function getViewportCenter(): FlowPosition {
    const viewport = flow.getViewport();
    const wrapper = document.querySelector(".react-flow");
    const w = wrapper?.clientWidth ?? 800;
    const h = wrapper?.clientHeight ?? 600;
    return {
      x: (w / 2 - viewport.x) / viewport.zoom,
      y: (h / 2 - viewport.y) / viewport.zoom,
    };
  }

  return { getViewportCenter };
}
