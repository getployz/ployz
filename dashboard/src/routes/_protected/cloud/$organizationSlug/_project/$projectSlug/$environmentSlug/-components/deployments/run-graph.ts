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

export function rowTiming(rows: ServerRow[]): RunTiming | undefined {
  if (!rows.length || rows.some((row) => row.started_at === null)) return undefined;
  const started = Math.min(...rows.map((row) => row.started_at ?? 0));
  const finished = rows.every((row) => row.finished_at !== null) ? Math.max(...rows.map((row) => row.finished_at ?? 0)) : null;
  return { started, finished };
}

/** An unfinished build runs only while its Deployment does; one left unfinished by an ended Deployment shows no time. */
function buildTiming(build: BuildView, inFlight: boolean): RunTiming | undefined {
  if (build.started_at === null || (build.finished_at === null && !inFlight)) return undefined;
  return { started: build.started_at, finished: build.finished_at };
}

const buildOutcome = {
  pending: "pending", building: "pending", built: "deployed", reused: "deployed", failed: "failed",
} satisfies Record<BuildStatus, NodeStatus>;

type Placeable = { id: string; name: string; column: number; rank: number };

/**
 * Each node's row so linked cards line up. A column goes by the mean row of the nodes linking into it, unlinked nodes
 * last by `rank`, ties by name.
 */
export function orderRows(nodes: Placeable[], edges: Pick<RunEdge, "source" | "target">[]): Map<string, number> {
  const rows = new Map<string, number>();
  const key = (node: Placeable) => {
    const linked = edges.filter((edge) => edge.target === node.id).flatMap((edge) => rows.get(edge.source) ?? []);
    return linked.length ? linked.reduce((sum, row) => sum + row, 0) / linked.length : Infinity;
  };
  const compare = (a: number | string, b: number | string) => (a < b ? -1 : a > b ? 1 : 0);
  for (const column of [...new Set(nodes.map((node) => node.column))].sort(compare)) {
    const keyed = nodes.filter((node) => node.column === column).map((node) => ({ node, key: key(node) }));
    keyed.sort((a, b) => compare(a.key, b.key) || compare(a.node.rank, b.node.rank) || compare(a.node.name, b.node.name));
    keyed.forEach(({ node }, index) => rows.set(node.id, index - (keyed.length - 1) / 2));
  }
  return rows;
}

const columnRanks = { build: 0, volume: 1, config: 2, service: 3 } satisfies Record<RunNode["type"], number>;

/**
 * A Deployment's Services at their preview depth, each build in the column just before its Service, and Volumes and
 * Configs in the first column, since nothing says which Service mounts them. Edges run from each build to its Service
 * and along the preview's dependencies. A deployed Service reads as running while any of its rows runs, old copies
 * stopping included, and an edge animates while its Service runs.
 */
export function makeRunGraph(deployment: DeploymentView): RunGraph {
  const runtime = (name: string) => deployment.runtime_names[name] ?? name;
  const runs = (node: NodeOutcome) =>
    node.type === "service" && deployment.in_flight && node.rows.some((row) => row.state === "running");
  const outcomes = deployment.nodes.map((node): OutcomeRunNode => ({
    type: node.type, id: node.id, name: node.name, node, row: 0, timing: rowTiming(node.rows),
    light: runs(node) && node.outcome === "deployed" ? "deploying" : nodeLight(node.outcome, deployment),
    column: node.type === "service" ? (deployment.preview?.depths[runtime(node.name)] ?? 0) + 1 : 0,
  }));
  const services = new Map(outcomes.filter((node) => node.type === "service").map((node) => [runtime(node.name), node]));
  const unplaced: RunNode[] = [
    ...deployment.builds.map((build): RunNode => ({
      type: "build", id: buildNodeId(build.service), name: build.service, build, row: 0,
      column: (services.get(runtime(build.service))?.column ?? 1) - 1,
      timing: buildTiming(build, deployment.in_flight), light: nodeLight(buildOutcome[build.status], deployment),
    })),
    ...outcomes,
  ];
  const links = [
    ...deployment.builds.map((build) => [buildNodeId(build.service), services.get(runtime(build.service))] as const),
    ...(deployment.preview?.dependencies ?? []).map(([dependency, dependent]) => [services.get(dependency)?.id, services.get(dependent)] as const),
  ];
  const edges = new Map<string, RunEdge>();
  for (const [source, target] of links) {
    if (source && target) {
      const id = `${source}->${target.id}`;
      edges.set(id, { id, source, target: target.id, animated: runs(target.node) });
    }
  }
  const rows = orderRows(
    unplaced.map(({ id, name, column, type }) => ({ id, name, column, rank: columnRanks[type] })),
    [...edges.values()],
  );
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
