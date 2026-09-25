import type { DeployOperation, ExecutionError, OperationPhase } from "@ployz/sdk";
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
  /** The failing container, when the failure names one. */
  containerId: text,
  /** Epoch ms the row was first seen started and finished; they give the Deploy stage its duration. */
  startedAt: Schema.NullOr(Schema.Number),
  finishedAt: Schema.NullOr(Schema.Number),
});
export const preparationProgressSchema = Schema.Struct({
  phase: Schema.Literals(["source", "selection", "build", "transfer", "ready"]),
  serviceId: text, machineId: text, machineName: text, message: text,
  failureCode: Schema.optional(Schema.String),
  stage: Schema.optional(Schema.String),
  work: Schema.optional(Schema.Record(Schema.String, Schema.String)),
});
export type PreparationProgress = typeof preparationProgressSchema.Type;
// Image Cleanup runs after the Deployment is terminal and never changes its status.
// While running, the row carries the pending targets as plain data.
export const imageCleanupProgressSchema = Schema.Union([
  Schema.Struct({
    state: Schema.Literal("running"), machines: Schema.Number,
    targets: Schema.Array(Schema.Struct({ machine_id: Schema.String, repository: Schema.String })),
  }),
  Schema.Struct({ state: Schema.Literals(["cleaned", "warning"]), machines: Schema.Number }),
]);
export type ImageCleanupProgress = typeof imageCleanupProgressSchema.Type;
export const deploymentProgressSchema = Schema.Struct({
  logsIncomplete: Schema.optional(Schema.Boolean),
  preparation: Schema.optional(preparationProgressSchema),
  imageCleanup: Schema.optional(imageCleanupProgressSchema),
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
