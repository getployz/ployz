import { useEffect, useRef, useState } from "react";
import {
  useNodesInitialized,
  useReactFlow,
  type ReactFlowInstance,
} from "@xyflow/react";
import {
  CANVAS_MIN_ZOOM,
  SERVICE_NODE_WIDTH,
  SERVICE_NODE_HEIGHT,
} from "./constants";
import type { CanvasResourceNode, FlowPosition } from "./types";

import { prefersReducedMotion } from "#/lib/motion";

const UNSET = Symbol("canvas-nav-unset");
const CANVAS_INSPECTOR_PANE_SELECTOR = "[data-canvas-inspector-pane]";
const CANVAS_INSPECTOR_FULL_WIDTH_RATIO = 0.9;

function getNodePositionKey(node: CanvasResourceNode) {
  return `${node.position.x}:${node.position.y}`;
}

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

export function shouldCenterSelectedNode(params: {
  selectedNode: CanvasResourceNode;
  selectedNodeId: string;
  previousSelectedNodeId: string | null | symbol;
  previousSelectedNodePositionKey: string | null;
}) {
  if (params.selectedNode.dragging) {
    return false;
  }

  if (params.previousSelectedNodeId !== params.selectedNodeId) {
    return true;
  }

  return (
    params.previousSelectedNodePositionKey !==
    getNodePositionKey(params.selectedNode)
  );
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

/** Brings nodes into the canvas area the inspector pane leaves visible. False while no pane leaves room for them. */
function revealNodes(flow: ReactFlowInstance<CanvasResourceNode>, nodes: readonly CanvasResourceNode[]) {
  const wrapper = document.querySelector<HTMLElement>(".react-flow");
  const pane = document.querySelector<HTMLElement>(CANVAS_INSPECTOR_PANE_SELECTOR);
  if (!wrapper || !pane || pane.dataset["takeover"] === "true") return false;
  if (pane.offsetWidth / wrapper.clientWidth >= CANVAS_INSPECTOR_FULL_WIDTH_RATIO) {
    return false;
  }
  if (!nodes.length) return true;
  const controls = wrapper.closest(".environment-canvas-scene")
    ?.querySelector<HTMLElement>(".bottom-bar");
  const x = Math.min(...nodes.map((node) => node.position.x));
  const y = Math.min(...nodes.map((node) => node.position.y));
  const box = {
    x, y,
    width: Math.max(...nodes.map((node) => node.position.x + (node.measured?.width ?? SERVICE_NODE_WIDTH))) - x,
    height: Math.max(...nodes.map((node) => node.position.y + (node.measured?.height ?? SERVICE_NODE_HEIGHT))) - y,
  };
  const viewport = flow.getViewport();
  const next = viewportShowing(viewport, box, wrapper.clientWidth - pane.offsetWidth,
    wrapper.clientHeight - (controls?.offsetHeight ?? 0), CANVAS_MIN_ZOOM);
  if (next.x !== viewport.x || next.y !== viewport.y || next.zoom !== viewport.zoom) {
    void flow.setViewport(next, { duration: prefersReducedMotion() ? 0 : 360 });
  }
  return true;
}

/**
 * Both canvases pass this as `onNodeClick`: React Flow gives a node pointer events only when it has a click handler
 * (or is selectable or draggable), so without it clicks fall through to the pane and the node's link never opens.
 * Mouse navigation must not leave a focus outline after the inspector closes.
 */
export function blurClickedNodeLink(event: Pick<MouseEvent, "detail" | "target">) {
  if (event.detail > 0 && event.target instanceof Element) {
    event.target.closest("a")?.blur();
  }
}

export function useCanvasNavigation(
  selectedNodeId: string | null,
  selectedNodePositionKey: string | null,
  flowReady: boolean,
  /** Nodes an open Deployment Page lights: brought into view once each time the set changes. */
  litNodeIds: readonly string[] | null,
) {
  const flow = useReactFlow<CanvasResourceNode>();
  const nodesInitialized = useNodesInitialized();
  const canvasInspectorGeometryVersion = useCanvasInspectorGeometryVersion(
    flowReady && selectedNodeId !== null,
  );
  const previousSelectedNodeId = useRef<string | null | symbol>(UNSET);
  const previousSelectedNodePositionKey = useRef<string | null>(null);
  const previousCanvasInspectorGeometryVersion = useRef(
    canvasInspectorGeometryVersion,
  );

  useEffect(() => {
    if (!flowReady) {
      return;
    }

    if (selectedNodeId === null) {
      if (previousSelectedNodeId.current !== UNSET && previousSelectedNodeId.current !== null) {
        void flow.setViewport(flow.getViewport(), { duration: 0 });
      }
      previousSelectedNodeId.current = null;
      previousSelectedNodePositionKey.current = null;
    }

    flow.setNodes((nodes) =>
      nodes.map((node) => {
        const nextSelected = node.id === selectedNodeId;

        if (node.selected === nextSelected) {
          return node;
        }

        return {
          ...node,
          selected: nextSelected,
        };
      }),
    );
  }, [flow, flowReady, nodesInitialized, selectedNodeId]);

  useEffect(() => {
    if (!flowReady || !selectedNodeId) {
      return;
    }

    const selectedNode = flow.getNode(selectedNodeId);
    if (!selectedNode) {
      return;
    }

    const shouldCenterForSelection = shouldCenterSelectedNode({
      selectedNode,
      selectedNodeId,
      previousSelectedNodeId: previousSelectedNodeId.current,
      previousSelectedNodePositionKey: previousSelectedNodePositionKey.current,
    });
    const shouldCenterForGeometry =
      !selectedNode.dragging &&
      previousCanvasInspectorGeometryVersion.current !==
        canvasInspectorGeometryVersion;

    if (!shouldCenterForSelection && !shouldCenterForGeometry) {
      previousSelectedNodeId.current = selectedNodeId;
      previousSelectedNodePositionKey.current = getNodePositionKey(selectedNode);
      previousCanvasInspectorGeometryVersion.current =
        canvasInspectorGeometryVersion;
      return;
    }

    function markSelectedNodeCentered(node: CanvasResourceNode) {
      previousSelectedNodeId.current = selectedNodeId;
      previousSelectedNodePositionKey.current = getNodePositionKey(node);
      previousCanvasInspectorGeometryVersion.current =
        canvasInspectorGeometryVersion;
    }

    if (revealNodes(flow, [selectedNode])) {
      markSelectedNodeCentered(selectedNode);
      return;
    }

    let cancelled = false;
    let frameId: number | null = window.requestAnimationFrame(() => {
      frameId = window.requestAnimationFrame(() => {
        frameId = null;
        if (cancelled) {
          return;
        }

        const nextSelectedNode = flow.getNode(selectedNodeId);
        if (nextSelectedNode && revealNodes(flow, [nextSelectedNode])) {
          markSelectedNodeCentered(nextSelectedNode);
        }
      });
    });

    return () => {
      cancelled = true;
      if (frameId != null) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [
    canvasInspectorGeometryVersion,
    flow,
    flowReady,
    nodesInitialized,
    selectedNodePositionKey,
    selectedNodeId,
  ]);

  // ponytail: ids joined into one key so the effect reruns only when the set changes; node ids never hold commas.
  const litKey = litNodeIds?.join(",") ?? null;
  useEffect(() => {
    if (!flowReady || litKey === null) return;
    const lit = () => litKey.split(",").flatMap((id) => flow.getNode(id) ?? []);
    if (revealNodes(flow, lit())) return;
    // The pane mounts with the page; measure once it has laid out.
    let frameId = window.requestAnimationFrame(() => {
      frameId = window.requestAnimationFrame(() => revealNodes(flow, lit()));
    });
    return () => window.cancelAnimationFrame(frameId);
  }, [flow, flowReady, nodesInitialized, litKey]);

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
