import { useEffect, type ReactNode } from "react";
import {
  BaseEdge, Background, BackgroundVariant, getViewportForBounds, Handle, Panel, Position, ReactFlow, useReactFlow, useStore,
  type Edge, type EdgeProps, type Node, type NodeProps, type Rect,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Link } from "@tanstack/react-router";
import { BoxIcon, BrushCleaningIcon, DatabaseIcon, FolderIcon, HammerIcon, type LucideIcon } from "lucide-react";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { RunningTime } from "#/components/relative-time";
import { Card, CardHeader, CardTitle, CardDescription } from "#/components/ui/card";
import { formatDuration } from "#/utils/relative-time";
import { cleanupLabel, type RunEdge, type RunGraph, type RunNode, type RunTiming } from "#/modules/config-store/deployments/run-graph";
import { DEPLOYMENT_ROUTE_TO } from "#/lib/environment-routes";

const NODE_WIDTH = 224;
const NODE_HEIGHT = 72;
const COLUMN_STEP = 272;
const ROW_STEP = 96;
const MIN_ZOOM = 0.25;
const FIT_PADDING = { top: "160px", left: "40px", right: "40px", bottom: "40px" } as const;

type RunFlowNode = Node<{ node: RunNode; selected: boolean }, "run">;

export const runNodeIcons = {
  build: HammerIcon, service: BoxIcon, volume: DatabaseIcon, config: FolderIcon, untargeted: BoxIcon, cleanup: BrushCleaningIcon,
} satisfies Record<RunNode["type"], LucideIcon>;
export const runNodeKinds = {
  build: "Build", service: "Service", volume: "Volume", config: "Config", untargeted: "Not in this environment", cleanup: "Cleanup",
} satisfies Record<RunNode["type"], string>;

function placed(graph: RunGraph, selectedId: string | undefined): RunFlowNode[] {
  return graph.nodes.map((node) => ({
    id: node.id, type: "run", data: { node, selected: node.id === selectedId },
    position: { x: node.column * COLUMN_STEP, y: node.row * ROW_STEP }, width: NODE_WIDTH, height: NODE_HEIGHT,
  }));
}

const EDGE_STYLE = { stroke: "var(--border)", strokeWidth: 1.5 };
const COLUMN_GUTTER = (COLUMN_STEP - NODE_WIDTH) / 2;
const ROW_GUTTER = (ROW_STEP - NODE_HEIGHT) / 2;
const CORNER_RADIUS = 8;

type DetourEdge = Edge<{ lane: number }, "detour">;

/**
 * The y of the gap an edge that skips columns runs along: just above or below every card in the columns it skips,
 * whichever is nearer its ends, so it never passes under a card it doesn't link. None when nothing sits between.
 */
export function detourLane(graph: RunGraph, edge: Pick<RunEdge, "source" | "target">): number | undefined {
  const source = graph.nodes.find((node) => node.id === edge.source);
  const target = graph.nodes.find((node) => node.id === edge.target);
  if (!source || !target) return undefined;
  const between = graph.nodes.filter((node) => node.column > source.column && node.column < target.column);
  if (!between.length) return undefined;
  const rows = between.map((node) => node.row);
  const above = Math.min(...rows) * ROW_STEP - ROW_GUTTER;
  const below = Math.max(...rows) * ROW_STEP + NODE_HEIGHT + ROW_GUTTER;
  const ends = [source.row, target.row].map((row) => row * ROW_STEP + NODE_HEIGHT / 2);
  const distance = (lane: number) => ends.reduce((sum, end) => sum + Math.abs(lane - end), 0);
  return distance(above) <= distance(below) ? above : below;
}

function linked(graph: RunGraph): Edge[] {
  return graph.edges.map((edge) => {
    const lane = detourLane(graph, edge);
    const shown = { ...edge, style: EDGE_STYLE, focusable: false, selectable: false };
    return lane === undefined ? shown : { ...shown, type: "detour", data: { lane } } satisfies DetourEdge;
  });
}

type Point = { x: number; y: number };

/** A rounded turn at `at`, coming from `from` and leaving for `to`, as SVG path commands. */
function corner(from: Point, at: Point, to: Point): string {
  const radius = Math.min(CORNER_RADIUS, Math.hypot(at.x - from.x, at.y - from.y) / 2, Math.hypot(to.x - at.x, to.y - at.y) / 2);
  const toward = (point: Point) => {
    const length = Math.hypot(point.x - at.x, point.y - at.y) || 1;
    return `${at.x + ((point.x - at.x) / length) * radius},${at.y + ((point.y - at.y) / length) * radius}`;
  };
  return `L${toward(from)} Q${at.x},${at.y} ${toward(to)}`;
}

/** Out into the gutter after its source, along its lane, down the gutter before its target. */
function DetourEdgePath({ sourceX, sourceY, targetX, targetY, data, style }: EdgeProps<DetourEdge>) {
  const lane = data?.lane ?? sourceY;
  const source = { x: sourceX, y: sourceY };
  const target = { x: targetX, y: targetY };
  const out = { x: sourceX + COLUMN_GUTTER, y: sourceY };
  const up = { x: out.x, y: lane };
  const across = { x: targetX - COLUMN_GUTTER, y: lane };
  const down = { x: across.x, y: targetY };
  const turns = [corner(source, out, up), corner(out, up, across), corner(up, across, down), corner(across, down, target)];
  return <BaseEdge path={`M${sourceX},${sourceY} ${turns.join(" ")} L${targetX},${targetY}`} style={style} />;
}

const edgeTypes = { detour: DetourEdgePath };

export function RunTime({ timing }: { timing: RunTiming | undefined }) {
  if (!timing) return null;
  return (
    <span className="shrink-0 text-muted-foreground tabular-nums">
      {timing.finished === null ? <RunningTime from={new Date(timing.started * 1000)} /> : formatDuration(timing.finished - timing.started)}
    </span>
  );
}

function caption(node: RunNode) {
  if (node.type === "cleanup") return cleanupLabel(node.cleanup);
  // A card has no room for a count after "Not in this environment"; its panel lists the Servers.
  if (node.type === "build" || node.type === "untargeted" || !node.node.rows.length) return runNodeKinds[node.type];
  const count = node.node.rows.length;
  return `${runNodeKinds[node.type]} · ${count} ${count === 1 ? "server" : "servers"}`;
}

function RunNodeCard({ data: { node, selected } }: NodeProps<RunFlowNode>) {
  const Icon = runNodeIcons[node.type];
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
            <Icon aria-hidden />{caption(node)}
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
      edgeTypes={edgeTypes}
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
