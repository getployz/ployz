import type { BuildStatus, BuildView, DeploymentView, FailedAt, NodeOutcome, NodeStatus, RowPhase, ServerRow } from "@ployz/sdk";
import { buildNodeId, nodeLight, type NodeLight } from "#/modules/config-store/store-deployments";
import type { RunSearch } from "./run-search";

/** Unix seconds over a node's rows; `finished` stays null until every row finished. */
export type RunTiming = { started: number; finished: number | null };

/** `row` is the node's offset from its column's middle, in card slots. */
type RunNodeBase = { id: string; name: string; light: NodeLight; column: number; row: number; timing?: RunTiming };
export type BuildRunNode = RunNodeBase & { type: "build"; build: BuildView };
export type OutcomeRunNode = RunNodeBase & { type: NodeOutcome["type"]; node: NodeOutcome };
/** One entry of a Deployment as it reports it: a build, or a Service, Volume or Config node. */
export type RunNode = BuildRunNode | OutcomeRunNode;

/** A build to its Service, or a Service to one depending on it; `animated` while the target runs. */
export type RunEdge = { id: string; source: string; target: string; animated: boolean };

export type RunGraph = { nodes: RunNode[]; edges: RunEdge[]; done: number };

export const buildStatusLabels = {
  pending: "Pending", building: "Building", built: "Built", reused: "Reused", failed: "Failed",
} satisfies Record<BuildStatus, string>;

export function rowPhaseLabel(phase: RowPhase): string {
  switch (phase) {
    case "starting": return "Starting";
    case "creating_container": return "Creating container";
    case "starting_container": return "Starting container";
    case "waiting_for_health": return "Waiting for health";
    case "waiting_for_hook": return "Waiting for hook";
    case "waiting_for_switch": return "Waiting for switch";
    case "stopping_container": return "Stopping container";
    case "removing_container": return "Removing container";
    case "removing_volume": return "Removing volume";
    case "compensating": return "Rolling back";
    default:
      return phase satisfies never;
  }
}

export const failedAtLabels = {
  build: "build", create: "create", start: "start", health: "health check", hook: "hook", stop: "stop", remove: "remove",
  volume: "volume",
} satisfies Record<FailedAt, string>;

export function rowLabel(row: ServerRow): string {
  switch (row.state) {
    case "pending": return "Pending";
    case "running": return rowPhaseLabel(row.phase);
    case "completed": return "Done";
    case "failed": return row.step ? `Failed at ${failedAtLabels[row.step]}` : "Failed";
    case "not_attempted": return "Not attempted";
    case "unknown": return "Unknown";
    default:
      return row satisfies never;
  }
}

export function rowLight(row: ServerRow): NodeLight {
  switch (row.state) {
    case "pending": return "queued";
    case "running": return "deploying";
    case "completed": return "deployed";
    case "failed": return "failed";
    case "not_attempted": return "not_applied";
    case "unknown": return "unknown";
    default:
      return row satisfies never;
  }
}

/** When a row's work started: its first state past pending, else what the row says. */
function rowStart(row: ServerRow): number | null {
  return row.history?.find((transition) => transition.state !== "pending")?.at ?? row.started_at;
}

export function rowTiming(rows: ServerRow[]): RunTiming | undefined {
  const starts = rows.map(rowStart);
  if (!starts.length || starts.some((start) => start === null)) return undefined;
  const started = Math.min(...starts.map((start) => start ?? 0));
  const finished = rows.every((row) => row.finished_at !== null) ? Math.max(...rows.map((row) => row.finished_at ?? 0)) : null;
  return { started, finished };
}

function buildTiming(build: BuildView): RunTiming | undefined {
  return build.started_at === null ? undefined : { started: build.started_at, finished: build.finished_at };
}

const buildOutcome = {
  pending: "pending", building: "pending", built: "deployed", reused: "deployed", failed: "failed",
} satisfies Record<BuildStatus, NodeStatus>;

type Placeable = { id: string; name: string; column: number; rank: number };

/**
 * Each node's row so linked cards line up. Column 0 goes by `rank`, then name. Every later column goes by the mean row
 * of the nodes linking into it, unlinked nodes last, ties by name.
 */
export function orderRows(nodes: Placeable[], edges: Pick<RunEdge, "source" | "target">[]): Map<string, number> {
  const rows = new Map<string, number>();
  const key = (node: Placeable) => {
    if (node.column === 0) return node.rank;
    const linked = edges.filter((edge) => edge.target === node.id).flatMap((edge) => rows.get(edge.source) ?? []);
    return linked.length ? linked.reduce((sum, row) => sum + row, 0) / linked.length : Infinity;
  };
  for (const column of [...new Set(nodes.map((node) => node.column))].sort((a, b) => a - b)) {
    const keyed = nodes.filter((node) => node.column === column).map((node) => ({ node, key: key(node) }));
    keyed.sort((a, b) => (a.key === b.key ? a.node.name.localeCompare(b.node.name) : a.key < b.key ? -1 : 1));
    keyed.forEach(({ node }, index) => rows.set(node.id, index - (keyed.length - 1) / 2));
  }
  return rows;
}

const columnRanks = { build: 0, volume: 1, config: 2, service: 3 } satisfies Record<RunNode["type"], number>;

/**
 * A Deployment's builds in the first column, then its nodes: a Service at its preview depth after the builds, a Volume or
 * Config beside the builds, since nothing says which Service mounts it. Edges run from each build to its Service and
 * along the preview's dependencies. A Service reads as running while any of its rows runs, old copies stopping included.
 */
export function makeRunGraph(deployment: DeploymentView): RunGraph {
  const runtime = (name: string) => deployment.runtime_names[name] ?? name;
  const unplaced: RunNode[] = [
    ...deployment.builds.map((build): RunNode => ({
      type: "build", id: buildNodeId(build.service), name: build.service, build, column: 0, row: 0, timing: buildTiming(build),
      light: nodeLight(buildOutcome[build.status], deployment),
    })),
    ...deployment.nodes.map((node): RunNode => ({
      type: node.type, id: node.id, name: node.name, node, row: 0, timing: rowTiming(node.rows),
      light: deployment.in_flight && node.rows.some((row) => row.state === "running") ? "deploying" : nodeLight(node.outcome, deployment),
      column: node.type === "service" ? (deployment.preview?.depths[runtime(node.name)] ?? 0) + 1 : 0,
    })),
  ];
  const services = new Map(deployment.nodes.filter((node) => node.type === "service").map((node) => [runtime(node.name), node.id]));
  const links = [
    ...deployment.builds.map((build) => [buildNodeId(build.service), services.get(runtime(build.service))] as const),
    ...(deployment.preview?.dependencies ?? []).map(([dependency, dependent]) => [services.get(dependency), services.get(dependent)] as const),
  ];
  const lights = new Map(unplaced.map((node) => [node.id, node.light]));
  const edges = new Map<string, RunEdge>();
  for (const [source, target] of links) {
    if (source && target) edges.set(`${source}->${target}`, { id: `${source}->${target}`, source, target, animated: lights.get(target) === "deploying" });
  }
  const rows = orderRows(unplaced.map((node) => ({ ...node, rank: columnRanks[node.type] })), [...edges.values()]);
  const nodes = unplaced.map((node) => ({ ...node, row: rows.get(node.id) ?? 0 }));
  return { nodes, edges: [...edges.values()], done: nodes.filter((node) => node.light === "deployed").length };
}

/**
 * The node the page opens on: the picked one, the one it was opened from, the first failed, the first with a running row
 * or building, the first still to run, the first Service.
 */
export function selectRunNode(graph: RunGraph, search: RunSearch): RunNode | undefined {
  const { nodes } = graph;
  const origin = search.origin?.node;
  return nodes.find((node) => node.id === search.job)
    ?? (origin && "id" in origin ? nodes.find((node) => node.type === origin.kind && node.id === origin.id) : undefined)
    ?? nodes.find((node) => node.light === "failed")
    ?? nodes.find((node) => node.type !== "build" && node.node.rows.some((row) => row.state === "running"))
    ?? nodes.find((node) => node.type === "build" && node.build.status === "building")
    ?? nodes.find((node) => node.light === "deploying")
    ?? nodes.find((node) => node.type === "service")
    ?? nodes[0];
}
