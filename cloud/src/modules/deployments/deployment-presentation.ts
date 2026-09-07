import type {
  DeployEvent,
  DeployOperation,
  OperationRow,
} from "@ployz/sdk";
import { asRecord, asString } from "#/lib/json";
import type { SdkDeployPreview } from "#/modules/deployments/runtime-preview";

export type SdkDeployProgressEvent = Extract<DeployEvent, { type: "progress" }>;

export function sdkDeployOperationKind(operation: DeployOperation): string {
  const type = asString(operation.type);
  if (type !== null && type.length > 0) return type;
  const record = asRecord(operation);
  return (record ? Object.keys(record)[0] : null) ?? "Operation";
}

function previewOperationRows(preview: SdkDeployPreview): OperationRow[] {
  // SAFETY: strict runtime decoding establishes the persisted rows before presentation.
  return preview.operations as OperationRow[];
}

function overlayRowStatus(
  row: OperationRow,
  status: OperationRow["status"],
): OperationRow {
  return { ...row, status };
}

export function stubPendingDeployProgress(
  preview: SdkDeployPreview,
): SdkDeployProgressEvent {
  const rows = previewOperationRows(preview);
  return {
    type: "progress",
    completed: 0,
    total: rows.length,
    rows: rows.map((row) => overlayRowStatus(row, { type: "pending" })),
  };
}

export function deployEventForDeployment(
  preview: SdkDeployPreview,
  status: string,
): SdkDeployProgressEvent {
  const pending = stubPendingDeployProgress(preview);
  if (status === "applied") {
    return {
      ...pending,
      completed: pending.total,
      rows: pending.rows.map((row) =>
        overlayRowStatus(row, { type: "completed" }),
      ),
    };
  }
  if (status === "failed") {
    return {
      ...pending,
      rows: pending.rows.map((row) =>
        overlayRowStatus(row, { type: "failed", error: { type: "cancelled" } }),
      ),
    };
  }
  if (status === "cancelled") {
    return {
      ...pending,
      rows: pending.rows.map((row) =>
        overlayRowStatus(row, { type: "unexecuted" }),
      ),
    };
  }
  return pending;
}
