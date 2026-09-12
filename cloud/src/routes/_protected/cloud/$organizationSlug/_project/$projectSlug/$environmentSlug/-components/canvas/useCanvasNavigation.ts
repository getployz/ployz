import { useEffect, useRef, useState } from "react";
import {
  useNodesInitialized,
  useReactFlow,
  type ReactFlowInstance,
} from "@xyflow/react";
import {
  SELECTED_SERVICE_ZOOM,
  SERVICE_NODE_WIDTH,
  SERVICE_NODE_HEIGHT,
} from "./constants";
import type { CanvasResourceNode, FlowPosition } from "./types";

const UNSET = Symbol("canvas-nav-unset");
const CANVAS_INSPECTOR_PANE_SELECTOR = "[data-canvas-inspector-pane]";
const CANVAS_INSPECTOR_FULL_WIDTH_RATIO = 0.9;

function getNodePositionKey(node: CanvasResourceNode) {
  return `${node.position.x}:${node.position.y}`;
}

export function getCanvasInspectorOffsetX(params: {
  flowWidth: number;
  paneWidth: number;
  zoom: number;
}) {
  if (params.flowWidth <= 0 || params.paneWidth <= 0) {
    return null;
  }

  if (params.paneWidth / params.flowWidth >= CANVAS_INSPECTOR_FULL_WIDTH_RATIO) {
    return null;
  }

  return params.paneWidth / 2 / params.zoom;
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

  return `${flowWidth}:${paneWidth}`;
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

function getOverlayOffsetX() {
  const wrapper = document.querySelector<HTMLElement>(".react-flow");
  const inspectorPane = document.querySelector<HTMLElement>(
    CANVAS_INSPECTOR_PANE_SELECTOR,
  );
  const flowWidth =
    wrapper?.getBoundingClientRect().width ?? window.innerWidth;
  const paneWidth = inspectorPane?.getBoundingClientRect().width ?? 0;

  return getCanvasInspectorOffsetX({
    flowWidth,
    paneWidth,
    zoom: SELECTED_SERVICE_ZOOM,
  });
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

function centerOnNode(
  flow: ReactFlowInstance<CanvasResourceNode>,
  node: CanvasResourceNode,
) {
  const offsetX = getOverlayOffsetX();
  if (offsetX == null) {
    return false;
  }

  const center = {
    x: node.position.x + SERVICE_NODE_WIDTH / 2 + offsetX,
    y: node.position.y + SERVICE_NODE_HEIGHT / 2,
  };

  void flow.setCenter(
    center.x,
    center.y,
    {
      duration: 350,
      zoom: SELECTED_SERVICE_ZOOM,
    },
  );

  return true;
}

function getCanvasNodesBounds(nodes: CanvasResourceNode[]) {
  const visibleNodes = nodes.filter((node) => !node.hidden);
  if (visibleNodes.length === 0) {
    return null;
  }

  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;

  for (const node of visibleNodes) {
    const width = node.width ?? node.measured?.width ?? SERVICE_NODE_WIDTH;
    const height = node.height ?? node.measured?.height ?? SERVICE_NODE_HEIGHT;

    minX = Math.min(minX, node.position.x);
    minY = Math.min(minY, node.position.y);
    maxX = Math.max(maxX, node.position.x + width);
    maxY = Math.max(maxY, node.position.y + height);
  }

  return {
    x: minX,
    y: minY,
    width: maxX - minX,
    height: maxY - minY,
  };
}

function fitCanvasToKnownNodeBounds(flow: ReactFlowInstance<CanvasResourceNode>) {
  const bounds = getCanvasNodesBounds(flow.getNodes());
  if (!bounds) {
    return false;
  }

  if ("fitBounds" in flow && flow.fitBounds instanceof Function) {
    void flow.fitBounds(bounds, {
      duration: 350,
      padding: 0.24,
    });
    return true;
  }

  void flow.fitView({
    duration: 350,
    padding: 0.24,
  });
  return true;
}

export function useCanvasNavigation(
  selectedNodeId: string | null,
  selectedNodePositionKey: string | null,
  flowReady: boolean,
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

  function onNodeClick(event: Pick<MouseEvent, "detail" | "target">) {
    // Mouse navigation must not leave a focus outline after the inspector closes.
    if (event.detail > 0 && event.target instanceof Element) {
      event.target.closest("a")?.blur();
    }
  }

  useEffect(() => {
    if (!flowReady) {
      return;
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

    if (centerOnNode(flow, selectedNode)) {
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
        if (nextSelectedNode && centerOnNode(flow, nextSelectedNode)) {
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

  useEffect(() => {
    if (!flowReady || selectedNodeId !== null) {
      return;
    }

    if (
      previousSelectedNodeId.current === UNSET ||
      previousSelectedNodeId.current === null
    ) {
      return;
    }

    let cancelled = false;
    let frameId: number | null = window.requestAnimationFrame(() => {
      frameId = null;
      if (cancelled) {
        return;
      }

      if (fitCanvasToKnownNodeBounds(flow)) {
        previousSelectedNodeId.current = null;
        previousSelectedNodePositionKey.current = null;
        previousCanvasInspectorGeometryVersion.current =
          canvasInspectorGeometryVersion;
      }
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
    selectedNodeId,
  ]);

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

  return { onNodeClick, getViewportCenter };
}
