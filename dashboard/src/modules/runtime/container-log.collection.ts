import type { Collection } from "@tanstack/react-db";
import { Schema } from "effect";
import type { LogRecord } from "@ployz/sdk";

const timestamp = Schema.String.check(Schema.isPattern(/^-?\d+$/));
export const containerLogRowSchema = Schema.Struct({
  id: Schema.String,
  timestamp: timestamp,
  machineId: Schema.String,
  machineName: Schema.String,
  containerId: Schema.String,
  serviceName: Schema.String,
  channel: Schema.Literals(["stdout", "stderr", "lifecycle"]),
  message: Schema.String,
});
export type ContainerLogRow = typeof containerLogRowSchema.Type;
export const logSourceErrorSchema = Schema.Struct({
  type: Schema.Literal("source_error"), machineId: Schema.String,
  containerId: Schema.String, message: Schema.String,
});
export const containerLogEventSchema = Schema.Union([
  Schema.Struct({ type: Schema.Literal("record"), record: containerLogRowSchema }),
  logSourceErrorSchema,
]);
export const containerLogPageSchema = Schema.Struct({
  records: Schema.Array(containerLogRowSchema), errors: Schema.Array(logSourceErrorSchema),
});

export function projectContainerLog(record: LogRecord): ContainerLogRow {
  if (record.source.origin.origin !== "service" || record.channel === "error") throw new Error("Expected container output");
  return {
    id: record.id, timestamp: record.timestamp_nanos,
    machineId: record.source.machine_id, machineName: record.source.machine_name,
    containerId: record.source.origin.container_id, serviceName: record.source.origin.service_name,
    channel: record.channel, message: record.message,
  };
}

export type ContainerLogs = Collection<ContainerLogRow, string>;

export function appendContainerLogs(collection: ContainerLogs, rows: readonly ContainerLogRow[]) {
  for (const row of rows) {
    if (!collection.has(row.id)) collection.insert(row);
  }
}

/** Each container starts with its own tail, so each needs its own history boundary. */
export function historyBoundaries(rows: readonly ContainerLogRow[]) {
  const before: Record<string, string> = {};
  for (const row of rows) {
    if (row.channel === "lifecycle") continue;
    const key = `${row.machineId}/${row.containerId}`;
    if (before[key] === undefined || BigInt(row.timestamp) < BigInt(before[key])) before[key] = row.timestamp;
  }
  return before;
}

/** Initial tails can begin inside a timestamp group; replace that group with the full read. */
export function mergeContainerHistory(collection: ContainerLogs, rows: readonly ContainerLogRow[]) {
  const groups = new Set(rows.map(row => `${row.machineId}/${row.containerId}/${row.timestamp}`));
  const removed = [...collection.values()].filter(row => groups.has(`${row.machineId}/${row.containerId}/${row.timestamp}`)).map(row => row.id);
  if (removed.length) collection.delete(removed);
  appendContainerLogs(collection, rows);
}

export function remainingHistory(rows: readonly ContainerLogRow[], exhausted: Record<string, string>) {
  return Object.fromEntries(Object.entries(historyBoundaries(rows)).filter(([source, before]) => exhausted[source] !== before));
}
