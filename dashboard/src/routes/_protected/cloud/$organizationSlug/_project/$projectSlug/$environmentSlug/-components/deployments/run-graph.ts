import type { BuildStatus, BuildView, DeploymentView, FailedAt, NodeOutcome, NodeStatus, RowPhase, ServerRow } from "@ployz/sdk";
import { buildNodeId, nodeLight, type NodeLight } from "#/modules/config-store/store-deployments";
import type { RunSearch } from "./run-search";

/** Unix seconds over a node's rows; `finished` stays null until every row finished. */
export type RunTiming = { started: number; finished: number | null };

type RunNodeBase = { id: string; name: string; light: NodeLight; column: number };
export type BuildRunNode = RunNodeBase & { type: "build"; build: BuildView };
export type OutcomeRunNode = RunNodeBase & { type: NodeOutcome["type"]; node: NodeOutcome; timing?: RunTiming };
/** One entry of a Deployment as it reports it: a build, or a Service, Volume or Config node. */
export type RunNode = BuildRunNode | OutcomeRunNode;

export type RunGraph = { nodes: RunNode[]; done: number };

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

const buildOutcome = {
  pending: "pending", building: "pending", built: "deployed", reused: "deployed", failed: "failed",
} satisfies Record<BuildStatus, NodeStatus>;

/**
 * A Deployment's builds in the first column, then its nodes: a Service at its preview depth after the builds, a Volume or
 * Config beside the builds, since the preview places only Services.
 */
export function makeRunGraph(deployment: DeploymentView): RunGraph {
  const builds: RunNode[] = deployment.builds.map((build) => ({
    type: "build", id: buildNodeId(build.service), name: build.service, build, column: 0,
    light: nodeLight(buildOutcome[build.status], deployment),
  }));
  const outcomes: RunNode[] = deployment.nodes.map((node) => ({
    type: node.type, id: node.id, name: node.name, node, light: nodeLight(node.outcome, deployment), timing: rowTiming(node.rows),
    column: node.type === "service" ? (deployment.preview?.depths[deployment.runtime_names[node.name] ?? node.name] ?? 0) + 1 : 0,
  }));
  const nodes = [...builds, ...outcomes];
  return { nodes, done: nodes.filter((node) => node.light === "deployed").length };
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
