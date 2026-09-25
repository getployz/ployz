import type { DeployEvent, DeployOperation, OperationRow } from "@ployz/sdk";
import { parseServiceConfig, type ServiceConfig } from "@ployz/sdk/config";
import { canonicalJson } from "#/modules/environment-design/canonical-json";
import { decodeStrict } from "#/modules/environment-design/schema";
import { persistedVolumeConfigSchema, type VolumeConfig } from "#/modules/environment-design/volume-config";
import type { EnvironmentDeploymentStatus } from "./tables";
import { BUILDING_KEY, CLEANUP_KEY, TRANSFER_KEY } from "./preparation-progress";
import { executionErrorLabel, progressRowLabel, type DeploymentProgress, type DeploymentProgressRow } from "./deployment-progress";
import { isActiveDeployment } from "./runtime-contract";

/**
 * The deployment view projection: the only place deployment state rules live.
 * Record side: `deploymentProgressForEvent` folds Engine events into durable progress.
 * Read side: `deploymentView` turns an attempt's data into what the UI renders.
 */

export type NodeOutcome = "deployed" | "removed" | "failed" | "not_attempted" | "unchanged";
/** `none` = prebuilt image. */
export type StageState = "queued" | "running" | "done" | "failed" | "skipped" | "none";
export type Stage = { state: StageState; durationMs?: number };
export type DeploymentNodeView = {
  nodeId: string;
  outcome: NodeOutcome | "queued" | "building" | "deploying";
  build: Stage;
  deploy: Stage;
  failure: { message: string; containerId: string | null } | null;
  tail: string[];
};
export type DeploymentViewStatus = "queued" | "building" | "deploying" | "deployed" | "failed" | "cancelled";
export type DeploymentView = {
  status: DeploymentViewStatus;
  deployed: number; changed: number;
  nodes: DeploymentNodeView[];
};
/** The attempt's Build Steps and their output, as the build log read returns them. */
type BuildStep = { id: number; build: number; key: string; name: string; startedAt: Date | null; completedAt: Date | null; error: string | null };
export type BuildLog = {
  steps: readonly BuildStep[];
  output: readonly { stepId: number; text: string }[];
};
export type DeploymentViewInput = {
  deployment: {
    status: EnvironmentDeploymentStatus;
    failureMessage: string | null;
    deployPreview: unknown;
  };
  progress: DeploymentProgress | null;
  nodes: readonly AttemptTargetNode[];
  buildLog?: BuildLog | null;
};

// Provider messages and operation specs may contain resolved secrets. Only
// explicit identity, lifecycle and structured failure facts cross this boundary.
function projectRow(row: OperationRow, serviceId: string | null): Omit<DeploymentProgressRow, "startedAt" | "finishedAt"> {
  const op = row.operation;
  const phase = row.status.type === "running" ? row.status.phase : null;
  const timed = phase?.type === "waiting_for_health" || phase?.type === "waiting_for_hook" ? phase : null;
  const target = "container_id" in op ? op.container_id : "old_container_id" in op ? op.old_container_id : op.type === "remove_volume" ? op.id.name : null;
  const error = row.status.type === "failed" ? row.status.error : null;
  const containerId = error?.type === "health" || error?.type === "hook" ? error.container_id : error && "container_id" in op ? op.container_id : null;
  return {
    index: row.index, machineId: row.machine_id, machineName: row.machine_name,
    serviceId,
    runtimeServiceId: "spec" in op ? op.spec.service_id : null,
    serviceName: row.service_name, displayName: row.display_name,
    operation: op.type,
    target,
    updateOrder: op.type === "replace_container" ? op.spec.update.order : null,
    status: row.status.type, phase: phase?.type ?? null,
    elapsedMs: timed?.elapsed_ms ?? null, deadlineMs: timed?.deadline_ms ?? null,
    health: phase?.type === "waiting_for_health" ? phase.health : null,
    error: error ? executionErrorLabel(error) : null,
    containerId,
  };
}

/**
 * Operations are matched structurally: the Engine serializes keys in its own
 * (alphabetical) order, so raw JSON of the planned and reported operation differs.
 * A failed row keeps the phase and deadline it was last seen running with.
 */
export function deploymentProgressForEvent(
  event: Exclude<DeployEvent, { type: "images_pruned" }>,
  planned: readonly OperationRow[],
  context: { prior?: DeploymentProgress; serviceIdFor?: (serviceName: string | null) => string | null; now?: number } = {},
): DeploymentProgress {
  const now = context.now ?? Date.now();
  const project = (row: OperationRow) => {
    const projected = projectRow(row, context.serviceIdFor?.(row.service_name) ?? null);
    const prior = context.prior?.rows.find((candidate) => candidate.index === row.index);
    const finished = projected.status === "completed" || projected.status === "failed";
    const timed = { ...projected,
      startedAt: prior?.startedAt ?? (finished || projected.status === "running" ? now : null),
      finishedAt: prior?.finishedAt ?? (finished ? now : null) };
    return projected.status === "failed" && prior ? { ...timed, phase: prior.phase, elapsedMs: prior.elapsedMs, deadlineMs: prior.deadlineMs, health: prior.health } : timed;
  };
  if (event.type === "progress") return { completed: event.completed, total: event.total, rows: event.rows.map(project), outcome: null, compensation: [] };
  const outcome = event.outcome;
  const completed = outcome.completed.map((op) => canonicalJson(op));
  const failedOp: DeployOperation | null = outcome.type === "failed"
    ? outcome.failed.type === "replacement_health" ? { type: "replace_container", ...outcome.failed.operation } : outcome.failed.operation
    : null;
  const failedKey = failedOp && canonicalJson(failedOp);
  let failureAssigned = false;
  const rows = planned.map((row) => {
    const key = canonicalJson(row.operation);
    const completedIndex = completed.indexOf(key);
    if (completedIndex !== -1) {
      completed.splice(completedIndex, 1);
      return project({ ...row, status: { type: "completed" } });
    }
    if (outcome.type === "failed" && key === failedKey && !failureAssigned) {
      failureAssigned = true;
      return project({ ...row, status: { type: "failed", error: outcome.failed.error } });
    }
    return project({ ...row, status: { type: "unexecuted" } });
  });
  const compensation: string[] = [];
  if (outcome.type === "failed" && outcome.failed.type === "replacement_health") {
    const c = outcome.failed.compensation;
    compensation.push(c.stop_new_container.type === "stopped" ? "Replacement container stopped" : `Could not stop replacement: ${executionErrorLabel(c.stop_new_container.error)}`);
    if (c.type === "stop_first") {
      compensation.push(c.restart_old_container.type === "restarted" ? "Previous container restarted" : c.restart_old_container.type === "not_attempted" ? "Previous container restart not attempted" : `Previous container restart failed: ${executionErrorLabel(c.restart_old_container.error)}`);
    }
  }
  return { completed: outcome.completed.length, total: rows.length, rows, outcome: outcome.type, compensation };
}

const TAIL_LINES = 2;
const rowLine = (row: DeploymentProgressRow) => `${row.machineName ?? row.machineId} · ${row.health ?? progressRowLabel(row)}${row.status === "completed" ? " · done" : ""}${row.elapsedMs !== null ? ` · ${Math.floor(row.elapsedMs / 1000)}s / ${Math.floor((row.deadlineMs ?? 0) / 1000)}s deadline` : ""}`;

/** Terminal colour and cursor sequences carry nothing the log needs. */
const ESC = String.fromCharCode(27);
const BEL = String.fromCharCode(7);
const ansi = new RegExp(`${ESC}(?:\\[[0-?]*[ -/]*[@-~]|\\][^${BEL}]*(?:${BEL}|${ESC}\\\\)|[@-Z\\\\-_])`, "g");
export const stripAnsi = (text: string) => text.replaceAll(ansi, "");
const logLines = (text: string) => stripAnsi(text).split("\n").filter((line) => line.trim());

/**
 * One Image Build's steps: the runs whose Building heading names the image (a multi-platform image has several),
 * without the attempt-wide cleanup and delivery filed under the last run. Build 0 (upload, builder preparation)
 * is shared: it counts only when it failed, because that failure stopped every image.
 */
export function imageBuildSteps<Step extends BuildStep>(steps: readonly Step[], image: string): Step[] {
  const runs = new Set(steps.filter((step) => step.key === BUILDING_KEY && step.name === image).map((step) => step.build));
  return steps.filter((step) => step.build === 0 ? step.error !== null : runs.has(step.build) && step.key !== CLEANUP_KEY && step.key !== TRANSFER_KEY);
}

type ImageBuild = { failed: boolean; open: boolean; durationMs: number | undefined; lines: string[]; errorLines: string[] };

function imageBuild(log: BuildLog, image: string): ImageBuild | null {
  const steps = imageBuildSteps(log.steps, image);
  if (!steps.length) return null;
  const failed = steps.filter((step) => step.error !== null);
  const output = (ids: ReadonlySet<number>) => logLines(log.output.filter((row) => ids.has(row.stepId)).map((row) => row.text).join(""));
  const starts = steps.flatMap((step) => step.startedAt ? [step.startedAt.getTime()] : []);
  const ends = steps.flatMap((step) => step.completedAt ? [step.completedAt.getTime()] : []);
  return {
    failed: failed.length > 0,
    open: steps.some((step) => step.startedAt && !step.completedAt),
    durationMs: starts.length && ends.length ? Math.max(...ends) - Math.min(...starts) : undefined,
    lines: output(new Set(steps.map((step) => step.id))),
    errorLines: [...output(new Set(failed.map((step) => step.id))), ...failed.flatMap((step) => logLines(step.error ?? ""))],
  };
}

/** What every node's rules read about the attempt as a whole. */
type AttemptFacts = {
  status: EnvironmentDeploymentStatus;
  active: boolean;
  /** The runtime reported no outcome: the attempt ended before or without one. */
  noOutcome: boolean;
  succeeded: boolean;
  failureMessage: string | null;
  /** The attempt's image preparation as a whole; each image refines it. */
  build: StageState;
  /** Every image is ready, or none was needed. */
  ready: boolean;
  /** Some image has a build run: images build one after another. */
  building: boolean;
  /** Some image's build failed, so a pre-runtime failure is pinned on it. */
  blamed: boolean;
};

function attemptBuildStage({ deployment, progress, nodes }: DeploymentViewInput, succeeded: boolean): StageState {
  // Prebuilt images only: nothing to build.
  if (!nodes.some((node) => node.image)) return "none";
  // Preparation said ready, the Engine planned the rollout, or the attempt succeeded: every image exists.
  if (progress?.preparation?.phase === "ready" || deployment.deployPreview || succeeded) return "done";
  if (deployment.status === "failed") return "failed";
  if (deployment.status === "cancelled") return "skipped";
  // Preparation reports progress once it starts; until then the attempt waits.
  return progress?.preparation ? "running" : "queued";
}

/** What the rules read about one node: its target entry, its Engine rows and its Image Build. */
type NodeFacts = {
  node: AttemptTargetNode;
  own: readonly DeploymentProgressRow[];
  failedRow: DeploymentProgressRow | undefined;
  image: ImageBuild | null;
  /** Every operation on the node completed, or the whole attempt succeeded. */
  done: boolean;
};

function nodeBuildStage({ node, image }: NodeFacts, attempt: AttemptFacts): Stage {
  // A prebuilt image has nothing to build.
  if (!node.image) return { state: "none" };
  if (image?.failed) return { state: "failed", durationMs: image.durationMs };
  // A run with no open step is done, and so is every image once the attempt's images are ready.
  if (image && (!image.open || attempt.build === "done")) return { state: "done", durationMs: image.durationMs };
  // An open run builds while the attempt is active; an ended attempt leaves it where preparation ended.
  if (image) return { state: attempt.active ? "running" : attempt.build };
  // No run yet: it waits while another image builds, and is skipped once another image's build failed.
  if (attempt.build === "running" && attempt.building) return { state: "queued" };
  if (attempt.build === "failed" && attempt.blamed) return { state: "skipped" };
  // Without a build log, the image follows the attempt's preparation.
  return { state: attempt.build };
}

/**
 * Without a runtime outcome, a failed attempt blames the image whose build failed,
 * or else every node it could have reached: built nodes, or all of them once images were ready.
 */
function failedBeforeRuntime({ node }: NodeFacts, build: Stage, attempt: AttemptFacts): boolean {
  if (attempt.status !== "failed" || !attempt.noOutcome) return false;
  if (attempt.blamed) return build.state === "failed";
  return node.image !== null || attempt.ready;
}

function nodeDeployStage({ node, own, done }: NodeFacts, preRuntimeFailure: boolean, attempt: AttemptFacts): Stage {
  // An unchanged node has nothing to roll out.
  if (!node.changed) return { state: "skipped" };
  const spans = own.flatMap((row) => row.startedAt !== null && row.finishedAt !== null ? [[row.startedAt, row.finishedAt] as const] : []);
  const durationMs = spans.length ? Math.max(...spans.map(([, end]) => end)) - Math.min(...spans.map(([start]) => start)) : undefined;
  if (own.some((row) => row.status === "failed")) return { state: "failed", durationMs };
  if (done) return { state: "done", durationMs };
  // An ended attempt either failed before the runtime reported on this node, or never reached it.
  if (!attempt.active) return { state: preRuntimeFailure && attempt.ready ? "failed" : "skipped" };
  if (own.some((row) => row.status === "running")) return { state: "running" };
  return { state: "queued" };
}

function nodeFailure({ node, failedRow, done }: NodeFacts, preRuntimeFailure: boolean, build: Stage, attempt: AttemptFacts): DeploymentNodeView["failure"] {
  // The Engine's failed operation names the error and, when it has one, the container.
  if (failedRow) return { message: failedRow.error ?? "Deployment failed", containerId: failedRow.containerId };
  // A pre-runtime failure carries the attempt's message.
  if (node.changed && !done && preRuntimeFailure) {
    return { message: attempt.failureMessage ?? (build.state === "failed" ? "Image preparation failed" : "Deployment failed"), containerId: null };
  }
  return null;
}

function nodeOutcome({ node, done }: NodeFacts, failure: DeploymentNodeView["failure"], build: Stage, deploy: Stage, attempt: AttemptFacts): DeploymentNodeView["outcome"] {
  if (!node.changed) return "unchanged";
  if (failure) return "failed";
  if (done) return node.removed ? "removed" : "deployed";
  // An ended attempt that neither finished nor failed this node never reached it.
  if (!attempt.active) return "not_attempted";
  if (build.state === "running") return "building";
  if (deploy.state === "running") return "deploying";
  return "queued";
}

/** The tail follows the stage that matters: the error when failed, else the rollout once it started, else the build. */
function nodeTail({ own, failedRow, image }: NodeFacts, failure: DeploymentNodeView["failure"], build: Stage): string[] {
  if (failure && failedRow) return [`${failedRow.machineName ?? failedRow.machineId} · ${failure.message}`];
  // A failed build shows its own output and error.
  if (failure && build.state === "failed" && image?.errorLines.length) return image.errorLines;
  if (failure) return [failure.message];
  const started = own.filter((row) => row.status === "running" || row.status === "completed");
  if (started.length) return started.map(rowLine);
  return image?.lines ?? [];
}

function attemptStatus(status: EnvironmentDeploymentStatus, ready: boolean): DeploymentViewStatus {
  if (status === "applied") return "deployed";
  if (status === "failed" || status === "cancelled" || status === "queued") return status;
  // Planning and deploying: images first, then the rollout.
  return ready ? "deploying" : "building";
}

export function deploymentView(input: DeploymentViewInput): DeploymentView {
  const { deployment, progress, nodes, buildLog } = input;
  const succeeded = progress?.outcome === "success" || deployment.status === "applied";
  const build = attemptBuildStage(input, succeeded);
  const images = new Map(nodes.map((node) => [node.nodeId, buildLog && node.image ? imageBuild(buildLog, node.image) : null]));
  const attempt: AttemptFacts = {
    status: deployment.status, active: isActiveDeployment(deployment.status), noOutcome: !progress?.outcome, succeeded,
    failureMessage: deployment.failureMessage, build, ready: build === "none" || build === "done",
    building: [...images.values()].some(Boolean), blamed: [...images.values()].some((image) => image?.failed),
  };
  const rows = progress?.rows ?? [];

  const views = nodes.map((node): DeploymentNodeView => {
    const own = rows.filter((row) => row.serviceId === node.nodeId);
    const facts: NodeFacts = {
      node, own, failedRow: own.find((row) => row.status === "failed"), image: images.get(node.nodeId) ?? null,
      done: (own.length > 0 && own.every((row) => row.status === "completed")) || succeeded,
    };
    const buildStage = nodeBuildStage(facts, attempt);
    const preRuntimeFailure = failedBeforeRuntime(facts, buildStage, attempt);
    const deploy = nodeDeployStage(facts, preRuntimeFailure, attempt);
    const failure = nodeFailure(facts, preRuntimeFailure, buildStage, attempt);
    return {
      nodeId: node.nodeId, outcome: nodeOutcome(facts, failure, buildStage, deploy, attempt),
      build: buildStage, deploy, failure, tail: nodeTail(facts, failure, buildStage).slice(-TAIL_LINES),
    };
  });

  const changed = views.filter((node) => node.outcome !== "unchanged");
  return {
    status: attemptStatus(deployment.status, attempt.ready),
    deployed: changed.filter((node) => node.outcome === "deployed" || node.outcome === "removed").length,
    changed: changed.length,
    nodes: views,
  };
}

type AttemptRow = { id: string; status: EnvironmentDeploymentStatus; createdAt: Date };
type SnapshotRow = { environmentDeploymentId: string; nodeType: "service" | "volume"; nodeId: string; config: unknown };
type ParsedSnapshot = { row: SnapshotRow } & ({ nodeType: "service"; config: ServiceConfig } | { nodeType: "volume"; config: VolumeConfig });
/**
 * One Environment Node of the Attempt Target with the configuration it was deployed (or last deployed, when removed) with,
 * parsed once. `image` names its Image Build (a git service's private DNS name); null when nothing is built.
 */
export type AttemptTargetNode = { nodeId: string; changed: boolean; removed: boolean; image: string | null } & (
  | { nodeType: "service"; config: ServiceConfig }
  | { nodeType: "volume"; config: VolumeConfig }
);

const parseSnapshot = (row: SnapshotRow): ParsedSnapshot => row.nodeType === "volume"
  ? { row, nodeType: "volume", config: decodeStrict(persistedVolumeConfigSchema, row.config) }
  : { row, nodeType: "service", config: parseServiceConfig(row.config) };

function targetNode(snapshot: ParsedSnapshot, changed: boolean, removed: boolean): AttemptTargetNode {
  const { nodeId } = snapshot.row;
  if (snapshot.nodeType === "volume") return { nodeId, nodeType: "volume", config: snapshot.config, image: null, changed, removed };
  const { config } = snapshot;
  // A removed service builds nothing.
  return { nodeId, nodeType: "service", config, image: config.source.type === "git" && !removed ? config.privateDns : null, changed, removed };
}

/**
 * The Attempt Target's full node set, read from deployment snapshots: every node the attempt froze,
 * plus the nodes the last applied attempt before it had and this one dropped (Removed).
 * Once the Engine reports rows, a service is changed only if it has operations; before that,
 * a node is changed when it is built or its snapshot differs from that applied attempt's.
 * Rows for removed services carry no serviceId from the record side, so they are resolved here by name.
 */
export function attemptTarget({ attempt, progress, history, snapshots }: {
  attempt: AttemptRow; progress: DeploymentProgress | null;
  history: readonly AttemptRow[]; snapshots: readonly SnapshotRow[];
}) {
  // ponytail: diffs against the last fully applied attempt; a failed attempt in between that deployed some nodes is ignored until rows arrive.
  const base = history.filter((row) => row.status === "applied" && row.createdAt < attempt.createdAt)
    .sort((a, b) => b.createdAt.getTime() - a.createdAt.getTime())[0];
  const own = snapshots.filter((row) => row.environmentDeploymentId === attempt.id);
  const prior = base ? snapshots.filter((row) => row.environmentDeploymentId === base.id) : [];
  const sameNode = (a: SnapshotRow) => (b: SnapshotRow) => a.nodeType === b.nodeType && a.nodeId === b.nodeId;
  const removed = prior.filter((row) => !own.some(sameNode(row))).map(parseSnapshot);
  const kept = own.map(parseSnapshot);
  const serviceFor = (name: string | null) => [...kept, ...removed]
    .find((snapshot) => snapshot.nodeType === "service" && snapshot.config.privateDns === name)?.row.nodeId ?? null;
  const resolved = progress && { ...progress, rows: progress.rows.map((row) => row.serviceId ? row : { ...row, serviceId: serviceFor(row.serviceName) }) };
  const rows = resolved?.rows ?? [];
  const changed = (snapshot: ParsedSnapshot) => {
    if (snapshot.nodeType === "service" && rows.length > 0) return rows.some((r) => r.serviceId === snapshot.row.nodeId);
    if (snapshot.nodeType === "service" && snapshot.config.source.type === "git") return true;
    const before = prior.find(sameNode(snapshot.row));
    return !before || canonicalJson(before.config) !== canonicalJson(snapshot.row.config);
  };
  return {
    nodes: [...kept.map((snapshot) => targetNode(snapshot, changed(snapshot), false)), ...removed.map((snapshot) => targetNode(snapshot, true, true))],
    progress: resolved,
  };
}

/** User-facing whole-deployment status: "Deployed" or "Failed · 2 of 4 deployed". Never "Partial". */
export function deploymentStatusLabel(view: DeploymentView): string {
  if (view.status === "failed") return `Failed · ${view.deployed} of ${view.changed} deployed`;
  return { queued: "Queued", building: "Building", deploying: "Deploying", deployed: "Deployed", cancelled: "Cancelled" }[view.status];
}

export const nodeOutcomeLabels = {
  deployed: "Deployed", removed: "Removed", failed: "Failed", not_attempted: "Not attempted", unchanged: "Unchanged",
  queued: "Queued", building: "Building", deploying: "Deploying",
} satisfies Record<DeploymentNodeView["outcome"], string>;

/** The first eight characters: how the UI names an attempt next to its message. */
export const shortDeploymentId = (id: string) => id.slice(0, 8);
