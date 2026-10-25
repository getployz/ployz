import type { BuildStatus, BuildView, DeploymentView, FailedAt, NodeOutcome, NodeStatus, RowPhase, ServerRow } from "@ployz/sdk";
import { buildNodeId, nodeLight, shownNodeStatus, type NodeLight, type ShownNodeStatus } from "#/modules/config-store/store-deployments";
import type { DeploymentCleanupState, DeploymentCleanupView } from "#/modules/config-store/tables";
import type { RunSearch } from "./run-search";

/** Unix seconds over a node's rows; `finished` stays null until every row finished. */
export type RunTiming = { started: number; finished: number | null };

/** `row` is the node's offset from its column's middle, in card slots. */
type RunNodeBase = { id: string; name: string; light: NodeLight; column: number; row: number; timing?: RunTiming };
export type BuildRunNode = RunNodeBase & { type: "build"; build: BuildView };
export type OutcomeRunNode = RunNodeBase & { type: NodeOutcome["type"]; node: NodeOutcome };
/** A runtime Service the Deployment removed that its Environment doesn't hold, so no node of its own targets it. */
export type UntargetedRunNode = RunNodeBase & { type: "untargeted"; status: ShownNodeStatus; rows: ServerRow[] };
/** Cloud retiring the old copies the Deployment replaced, after its Services. */
export type CleanupRunNode = RunNodeBase & { type: "cleanup"; cleanup: DeploymentCleanupView };
/**
 * One entry of a Deployment as it reports it: a build, a Service, Volume or Config node, or a Service it removed that
 * its Environment doesn't hold; then Cloud's cleanup.
 */
export type RunNode = BuildRunNode | OutcomeRunNode | UntargetedRunNode | CleanupRunNode;

/** A build to its Service, or a Service to one depending on it; `animated` while the target runs. */
export type RunEdge = { id: string; source: string; target: string; animated: boolean };

/** `done` of `total` counts the Deployment's own steps, never Cloud's cleanup, which joins only once the run ended. */
export type RunGraph = { nodes: RunNode[]; edges: RunEdge[]; done: number; total: number };

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
    case "updating_container": return "Updating limits";
    case "compensating": return "Rolling back";
    default:
      return phase satisfies never;
  }
}

export const failedAtLabels = {
  build: "build", pull: "pull", create: "create", start: "start", health: "health check", hook: "hook", stop: "stop", remove: "remove",
  volume: "volume",
} satisfies Record<FailedAt, string>;

/** Work Ployz stopped rather than work that failed: the Deployment's cancel, or the Deployment losing its runner. */
export const rowStopped = (row: ServerRow) => row.state === "failed" && (row.cancelled === true || row.interrupted === true);

export function rowLabel(row: ServerRow): string {
  switch (row.state) {
    case "pending": return "Pending";
    case "running": return rowPhaseLabel(row.phase);
    case "completed": return "Done";
    case "failed":
      if (row.cancelled) return "Cancelled";
      if (row.interrupted) return "Interrupted";
      return row.step ? `Failed at ${failedAtLabels[row.step]}` : "Failed";
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
    case "failed": return rowStopped(row) ? "not_applied" : "failed";
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

const columnRanks = { build: 0, volume: 1, config: 2, service: 3, untargeted: 4, cleanup: 5 } satisfies Record<RunNode["type"], number>;

const CLEANUP_NODE_ID = "cleanup";

const cleanupLights = {
  waiting: "deploying", done: "deployed", left_running: "unknown", cancelled: "not_applied", failed: "failed",
} satisfies Record<DeploymentCleanupState, NodeLight>;

/** How removing a Service no node targets went, read from its rows since the Store records no outcome for it. */
function untargetedStatus(rows: ServerRow[]): ShownNodeStatus {
  if (rows.some((row) => row.state === "failed" && !rowStopped(row))) return "failed";
  if (rows.some((row) => row.state === "failed" && row.cancelled === true)) return "cancelled";
  if (rows.some((row) => row.state === "unknown" || (row.state === "failed" && row.interrupted === true))) return "unknown";
  if (rows.some((row) => row.state === "pending" || row.state === "running")) return "pending";
  return rows.every((row) => row.state === "completed") ? "removed" : "not_attempted";
}

export function cleanupLabel(cleanup: DeploymentCleanupView): string {
  switch (cleanup.state) {
    case "waiting": return "Waiting";
    case "done": return "Done";
    case "left_running": return `Left running on ${[...new Set(cleanup.left.map((left) => left.machine))].join(", ")}`;
    case "cancelled": return "A newer Deployment took over";
    case "failed": return "Failed";
    default:
      return cleanup.state satisfies never;
  }
}

/**
 * A Deployment's Services at their preview depth, each build in the column just before its Service, and Volumes and
 * Configs in the first column, since nothing says which Service mounts them. Edges run from each build to its Service
 * and along the preview's dependencies. A deployed Service reads as running while any of its rows runs, old copies
 * stopping included, and an edge animates while its Service runs. Each Service it removed that its Environment doesn't
 * hold sits unlinked at its own preview depth. Cloud's `cleanup`, when it ran one, follows last.
 */
export function makeRunGraph(deployment: DeploymentView, cleanup: DeploymentCleanupView | null = null): RunGraph {
  const runtime = (name: string) => deployment.runtime_names[name] ?? name;
  const runs = (node: NodeOutcome) =>
    node.type === "service" && deployment.in_flight && node.rows.some((row) => row.state === "running");
  const outcomes = deployment.nodes.map((node): OutcomeRunNode => ({
    type: node.type, id: node.id, name: node.name, node, row: 0, timing: rowTiming(node.rows),
    light: runs(node) && node.outcome === "deployed" ? "deploying" : nodeLight(shownNodeStatus(node), deployment),
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
    ...Object.entries(deployment.untargeted ?? {}).map(([name, rows]): RunNode => {
      const status = untargetedStatus(rows);
      return {
        type: "untargeted", id: `untargeted:${name}`, name, status, rows, row: 0, timing: rowTiming(rows),
        light: nodeLight(status, deployment), column: (deployment.preview?.depths[name] ?? 0) + 1,
      };
    }),
  ];
  if (cleanup !== null) {
    unplaced.push({
      type: "cleanup", id: CLEANUP_NODE_ID, name: "Cleanup", cleanup, row: 0, light: cleanupLights[cleanup.state],
      column: Math.max(-1, ...unplaced.map((node) => node.column)) + 1,
    });
  }
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
  const steps = nodes.filter((node) => node.type !== "cleanup");
  return { nodes, edges: [...edges.values()], done: steps.filter((node) => node.light === "deployed").length, total: steps.length };
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
    ?? nodes.find((node) => "node" in node && node.node.rows.some((row) => row.state === "running"))
    ?? nodes.find((node) => node.type === "build" && node.build.status === "building")
    ?? nodes.find((node) => node.light === "deploying")
    ?? nodes.find((node) => node.type === "service")
    ?? nodes[0];
}
