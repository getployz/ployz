import type { DeployEvent, DeployOperation, ExecutionError, OperationRow, OperationPhase } from "@ployz/sdk";
import { Schema } from "effect";

const text = Schema.NullOr(Schema.String);
export const deploymentProgressRowSchema = Schema.Struct({
  index: Schema.Int,
  machineId: Schema.String,
  machineName: text,
  serviceId: text,
  runtimeServiceId: text,
  serviceName: text,
  displayName: text,
  operation: Schema.String,
  target: text,
  updateOrder: text,
  status: Schema.Literals(["pending", "running", "completed", "failed", "unexecuted"]),
  phase: text,
  elapsedMs: Schema.NullOr(Schema.Number),
  deadlineMs: Schema.NullOr(Schema.Number),
  health: text,
  error: text,
});
export const deploymentProgressSchema = Schema.Struct({
  completed: Schema.Number,
  total: Schema.Number,
  outcome: Schema.NullOr(Schema.Literals(["success", "failed"])),
  rows: Schema.Array(deploymentProgressRowSchema),
  compensation: Schema.Array(Schema.String),
});
export type DeploymentProgress = typeof deploymentProgressSchema.Type;
export type DeploymentProgressRow = typeof deploymentProgressRowSchema.Type;

// Provider messages and operation specs may contain resolved secrets. Only
// explicit identity, lifecycle and structured failure facts cross this boundary.
export function executionErrorLabel(error: ExecutionError): string {
  switch (error.type) {
    case "cancelled": return "Cancelled";
    case "machine": return `${error.action}: ${error.error.code}`;
    case "health": return `Health check ${error.failure.type.replaceAll("_", " ")}${error.failure.type === "runtime" ? ` · ${error.failure.observation.state} · ${"health" in error.failure.observation ? error.failure.observation.health : "no health observation"}` : ""}`;
    case "hook": return error.failure.type === "exit" ? `Pre-deploy command exited with code ${error.failure.code}` : `Pre-deploy command ${error.failure.type.replaceAll("_", " ")}`;
    case "dependency_health": return `Dependency ${error.dependency} · ${error.failure.type.replaceAll("_", " ")}`;
  }
}

function projectRow(row: OperationRow): DeploymentProgressRow {
  const op = row.operation;
  const phase = row.status.type === "running" ? row.status.phase : null;
  const timed = phase?.type === "waiting_for_health" || phase?.type === "waiting_for_hook" ? phase : null;
  return {
    index: row.index, machineId: row.machine_id, machineName: row.machine_name,
    serviceId: null,
    runtimeServiceId: "spec" in op ? op.spec.service_id : null,
    serviceName: row.service_name, displayName: row.display_name,
    operation: op.type,
    target: "container_id" in op ? op.container_id : "old_container_id" in op ? op.old_container_id : op.type === "remove_volume" ? op.id.name : null,
    updateOrder: op.type === "replace_container" ? op.spec.update.order : null,
    status: row.status.type, phase: phase?.type ?? null,
    elapsedMs: timed?.elapsed_ms ?? null, deadlineMs: timed?.deadline_ms ?? null,
    health: phase?.type === "waiting_for_health" ? phase.health : null,
    error: row.status.type === "failed" ? executionErrorLabel(row.status.error) : null,
  };
}

export function deploymentProgressForEvent(event: DeployEvent, planned: readonly OperationRow[]): DeploymentProgress {
  if (event.type === "progress") return { completed: event.completed, total: event.total, rows: event.rows.map(projectRow), outcome: null, compensation: [] };
  const outcome = event.outcome;
  const completed = outcome.completed.map((op) => JSON.stringify(op));
  const failedOp: DeployOperation | null = outcome.type === "failed"
    ? outcome.failed.type === "replacement_health" ? { type: "replace_container", ...outcome.failed.operation } : outcome.failed.operation
    : null;
  const failedKey = failedOp && JSON.stringify(failedOp);
  let failureAssigned = false;
  const rows = planned.map((row) => {
    const key = JSON.stringify(row.operation);
    const completedIndex = completed.indexOf(key);
    if (completedIndex !== -1) {
      completed.splice(completedIndex, 1);
      return projectRow({ ...row, status: { type: "completed" } });
    }
    if (outcome.type === "failed" && key === failedKey && !failureAssigned) {
      failureAssigned = true;
      return projectRow({ ...row, status: { type: "failed", error: outcome.failed.error } });
    }
    return projectRow({ ...row, status: { type: "unexecuted" } });
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

export const operationLabels = {
  prepare_volumes: "Preparing volumes", wait_healthy: "Waiting for dependency",
  run_container: "Starting replica", replace_container: "Updating replica",
  stop_container: "Stopping container", remove_container: "Removing container",
  run_hook: "Running pre-deploy command", stop_hook: "Stopping pre-deploy container", remove_volume: "Removing volume",
} satisfies Record<DeployOperation["type"], string>;
export const phaseLabels = {
  starting: "Starting", creating_container: "Creating container", starting_container: "Starting container",
  waiting_for_health: "Checking health", waiting_for_hook: "Running pre-deploy command",
  stopping_container: "Stopping container", removing_container: "Removing container", removing_volume: "Removing volume", compensating: "Recovering failed replacement",
} satisfies Record<OperationPhase["type"], string>;
export function progressRowLabel(row: DeploymentProgressRow) {
  return row.error ?? Object.entries(phaseLabels).find(([phase]) => phase === row.phase)?.[1] ?? Object.entries(operationLabels).find(([operation]) => operation === row.operation)?.[1] ?? row.operation;
}
