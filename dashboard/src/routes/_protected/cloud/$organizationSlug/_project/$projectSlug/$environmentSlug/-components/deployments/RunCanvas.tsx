import { useEffect, type ReactNode } from "react";
import {
  Background, BackgroundVariant, getViewportForBounds, Handle, Panel, Position, ReactFlow, useReactFlow, useStore,
  type Edge, type Node, type NodeProps, type Rect,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Link } from "@tanstack/react-router";
import { BoxIcon, DatabaseIcon, FolderIcon, HammerIcon, type LucideIcon } from "lucide-react";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { RunningTime } from "#/components/relative-time";
import { Card, CardHeader, CardTitle, CardDescription } from "#/components/ui/card";
import { formatDuration } from "#/utils/relative-time";
import type { RunGraph, RunNode, RunTiming } from "./run-graph";
import { DEPLOYMENT_ROUTE_TO } from "./run-search";

const NODE_WIDTH = 224;
const NODE_HEIGHT = 72;
const COLUMN_STEP = 272;
const ROW_STEP = 96;
const MIN_ZOOM = 0.25;
const FIT_PADDING = { top: "160px", left: "40px", right: "40px", bottom: "40px" } as const;

type RunFlowNode = Node<{ node: RunNode; selected: boolean }, "run">;

export const runNodeIcons = { build: HammerIcon, service: BoxIcon, volume: DatabaseIcon, config: FolderIcon } satisfies
  Record<RunNode["type"], LucideIcon>;
export const runNodeKinds = { build: "Build", service: "Service", volume: "Volume", config: "Config" } satisfies Record<RunNode["type"], string>;

function placed(graph: RunGraph, selectedId: string | undefined): RunFlowNode[] {
  return graph.nodes.map((node) => ({
    id: node.id, type: "run", data: { node, selected: node.id === selectedId },
    position: { x: node.column * COLUMN_STEP, y: node.row * ROW_STEP }, width: NODE_WIDTH, height: NODE_HEIGHT,
  }));
}

const EDGE_STYLE = { stroke: "var(--border)", strokeWidth: 1.5 };

function linked(graph: RunGraph): Edge[] {
  return graph.edges.map((edge) => ({ ...edge, style: EDGE_STYLE, focusable: false, selectable: false }));
}

export function RunTime({ timing }: { timing: RunTiming | undefined }) {
  if (!timing) return null;
  return (
    <span className="shrink-0 font-mono text-muted-foreground tabular-nums">
      {timing.finished === null ? <RunningTime from={new Date(timing.started * 1000)} /> : formatDuration(timing.finished - timing.started)}
    </span>
  );
}

function servers(node: RunNode) {
  if (node.type === "build" || !node.node.rows.length) return null;
  const count = node.node.rows.length;
  return `${count} ${count === 1 ? "server" : "servers"}`;
}

function RunNodeCard({ data: { node, selected } }: NodeProps<RunFlowNode>) {
  const Icon = runNodeIcons[node.type];
  const on = servers(node);
  return (
    <Link from={DEPLOYMENT_ROUTE_TO} to="." search={(previous) => ({ ...previous, job: node.id })} replace
      data-canvas-node={node.id} aria-current={selected ? "true" : undefined} draggable={false} className="block size-full rounded-xl">
      <Card size="node" data-selected={selected} className="h-full justify-center">
        <CardHeader className="group-data-[size=node]/card:px-4 group-data-[size=node]/card:pt-0">
          <div className="flex min-w-0 items-center gap-2 [&_svg]:size-3.5">
            <DeploymentStatusIcon status={node.light} />
            <CardTitle className="min-w-0 flex-1">{node.name}</CardTitle>
            <RunTime timing={node.timing} />
          </div>
          <CardDescription className="flex items-center gap-1.5 [&_svg]:size-3">
            <Icon aria-hidden />{runNodeKinds[node.type]}{on ? ` · ${on}` : null}
          </CardDescription>
        </CardHeader>
      </Card>
      <Handle type="target" position={Position.Left} isConnectable={false} className="invisible" />
      <Handle type="source" position={Position.Right} isConnectable={false} className="invisible" />
    </Link>
  );
}

const nodeTypes = { run: RunNodeCard };

function bounds(nodes: RunFlowNode[]): Rect {
  const xs = nodes.map((node) => node.position.x);
  const ys = nodes.map((node) => node.position.y);
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return { x, y, width: Math.max(...xs) + NODE_WIDTH - x, height: Math.max(...ys) + NODE_HEIGHT - y };
}

/**
 * Fits the view to the nodes whenever the canvas resizes or the columns change, as a live Run's depths arrive. Not
 * `fitView`: it fits once, often before the canvas has its final size.
 */
function FitRun({ x, y, width, height }: Rect) {
  const canvasWidth = useStore((state) => state.width);
  const canvasHeight = useStore((state) => state.height);
  const { setViewport } = useReactFlow();
  useEffect(() => {
    if (!canvasWidth || !canvasHeight) return;
    void setViewport(getViewportForBounds({ x, y, width, height }, canvasWidth, canvasHeight, MIN_ZOOM, 1, FIT_PADDING));
  }, [x, y, width, height, canvasWidth, canvasHeight, setViewport]);
  return null;
}

/** A Deployment's builds and nodes on a canvas, the header floating top left; a click picks the node the panel shows. */
export function RunCanvas({ graph, selectedId, header }: { graph: RunGraph; selectedId: string | undefined; header: ReactNode }) {
  const nodes = placed(graph, selectedId);
  return (
    <ReactFlow
      nodes={nodes}
      edges={linked(graph)}
      nodeTypes={nodeTypes}
      minZoom={MIN_ZOOM}
      nodesDraggable={false}
      nodesConnectable={false}
      elementsSelectable={false}
      nodesFocusable={false}
      proOptions={{ hideAttribution: true }}
      // React Flow gives a node pointer events only when it has a click handler (or is selectable), so without one a
      // mouse click falls through to the pane and the card's link never opens.
      onNodeClick={() => {}}
    >
      <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
      {nodes.length ? <FitRun {...bounds(nodes)} /> : null}
      <Panel position="top-left" className="max-w-[calc(100%-2rem)]">{header}</Panel>
    </ReactFlow>
  );
}
