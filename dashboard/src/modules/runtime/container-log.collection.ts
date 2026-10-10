import type { Collection } from "@tanstack/react-db";
import { Schema } from "effect";
import type { LogExit, LogGap, LogRecord } from "@ployz/sdk";

const timestamp = Schema.String.check(Schema.isPattern(/^-?\d+$/));
const source = {
  id: Schema.String,
  timestamp: timestamp,
  machineId: Schema.String,
  machineName: Schema.String,
  containerId: Schema.String,
  serviceName: Schema.String,
};
export const containerLogLineSchema = Schema.Struct({
  kind: Schema.Literal("line"),
  ...source,
  channel: Schema.Literals(["stdout", "stderr", "lifecycle"]),
  level: Schema.Literals(["error", "warn", "info", "debug"]),
  message: Schema.String,
});
/** A stretch of one container's output the Log Store doesn't have; `timestamp` is where it starts. */
export const containerLogGapSchema = Schema.Struct({
  kind: Schema.Literal("gap"),
  ...source,
  until: timestamp,
  reason: Schema.Literals(["not_captured", "corrupt"]),
});
export const containerLogRowSchema = Schema.Union([containerLogLineSchema, containerLogGapSchema]);
export type ContainerLogLine = typeof containerLogLineSchema.Type;
export type ContainerLogRow = typeof containerLogRowSchema.Type;
export const logSourceErrorSchema = Schema.Struct({
  type: Schema.Literal("source_error"), machineId: Schema.String,
  containerId: Schema.String, message: Schema.String,
});
export const containerLogEventSchema = Schema.Union([
  Schema.Struct({ type: Schema.Literal("record"), record: containerLogLineSchema }),
  logSourceErrorSchema,
  Schema.Struct({ type: Schema.Literal("source_gone"), machineId: Schema.String, containerId: Schema.String }),
]);
export const missingServerSchema = Schema.Struct({ machineId: Schema.String, machineName: Schema.String, message: Schema.String });
export type MissingServer = typeof missingServerSchema.Type;
export const containerLogPageSchema = Schema.Struct({
  rows: Schema.Array(containerLogRowSchema), failures: Schema.Array(missingServerSchema), cursor: Schema.NullOr(Schema.String),
});

export function projectContainerLog(record: LogRecord): ContainerLogLine {
  if (record.source.origin.origin !== "service" || record.channel === "error") throw new Error("Expected container output");
  return {
    kind: "line", id: record.id, timestamp: record.timestamp_nanos,
    machineId: record.source.machine_id, machineName: record.source.machine_name,
    containerId: record.source.origin.container_id, serviceName: record.source.origin.service_name,
    channel: record.channel, level: record.level, message: record.message,
  };
}

export function projectLogGap(gap: LogGap): ContainerLogRow {
  return {
    kind: "gap", id: `${gap.machineId}/${gap.containerId}/gap/${gap.fromNanos}`, timestamp: gap.fromNanos, until: gap.toNanos,
    machineId: gap.machineId, machineName: gap.machineName, containerId: gap.containerId, serviceName: gap.serviceName, reason: gap.reason,
  };
}

export function projectLogExit(exit: LogExit): ContainerLogLine {
  const code = exit.exitCode === null ? "" : ` (exit code ${exit.exitCode})`;
  return {
    kind: "line", id: `${exit.machineId}/${exit.containerId}/exit/${exit.timestampNanos}`, timestamp: exit.timestampNanos,
    machineId: exit.machineId, machineName: exit.machineName, containerId: exit.containerId, serviceName: exit.serviceName,
    channel: "lifecycle", level: exit.oomKilled || (exit.exitCode ?? 0) !== 0 ? "error" : "info",
    message: exit.oomKilled ? `Out of memory${code}` : exit.exitCode === null ? "Exited" : `Exited with code ${exit.exitCode}`,
  };
}

export type ContainerLogs = Collection<ContainerLogRow, string>;

const group = (row: ContainerLogRow) => `${row.machineId}/${row.containerId}/${row.timestamp}`;

/**
 * One insert for the whole batch: a live query recomputes once, not once per line. A live line whose timestamp group
 * the Log Store already answered is a repeat of it, as a reconnect's replayed tail is.
 */
export function appendContainerLogs(collection: ContainerLogs, rows: readonly ContainerLogRow[], stored: ReadonlySet<string> = new Set()) {
  const fresh = new Map(rows.filter(row => !collection.has(row.id) && !stored.has(group(row))).map(row => [row.id, row]));
  if (fresh.size) collection.insert([...fresh.values()]);
}

export const LIVE_LOG_LIMIT = 10_000;

/**
 * Drop the oldest lines back to `limit`, so a service printing thousands a second can't grow the page without end. It
 * waits for 10% slack first, so the sort runs once per thousand lines, not on every batch.
 */
export function trimContainerLogs(collection: ContainerLogs, limit = LIVE_LOG_LIMIT) {
  if (collection.size <= limit * 1.1) return false;
  const over = collection.size - limit;
  const oldest = [...collection.values()].map(row => ({ id: row.id, at: BigInt(row.timestamp) }))
    .sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : 0)).slice(0, over).map(row => row.id);
  collection.delete(oldest);
  return true;
}

/**
 * Live lines that waited past the limit lost their oldest, so what the page had can't meet them without a hole. The
 * page keeps only what's as new as the oldest one left.
 */
export function restartContainerLogs(collection: ContainerLogs, waiting: readonly ContainerLogRow[]) {
  const from = waiting.reduce<bigint | null>((min, row) => (min === null || BigInt(row.timestamp) < min ? BigInt(row.timestamp) : min), null);
  const older = [...collection.values()].filter(row => from === null || BigInt(row.timestamp) < from).map(row => row.id);
  if (older.length) collection.delete(older);
}

/**
 * The live tail starts each container somewhere inside the Log Store's newest page, so the two overlap. The Store's
 * read of a timestamp group replaces the tail's copy of it, and `stored` remembers the group for later live lines.
 */
export function mergeContainerHistory(collection: ContainerLogs, rows: readonly ContainerLogRow[], stored: Set<string>) {
  const page = new Set(rows.map(row => row.id));
  const groups = new Set(rows.filter(row => row.kind === "line").map(group));
  const replaced = [...collection.values()].filter(row => row.kind === "line" && !page.has(row.id) && !stored.has(group(row)) && groups.has(group(row))).map(row => row.id);
  if (replaced.length) collection.delete(replaced);
  for (const key of groups) stored.add(key);
  const fresh = rows.filter(row => !collection.has(row.id));
  if (fresh.length) collection.insert(fresh);
}

export const compareLogRows = (a: ContainerLogRow, b: ContainerLogRow) => {
  const difference = BigInt(a.timestamp) - BigInt(b.timestamp);
  return difference < 0n ? -1 : difference > 0n ? 1 : a.id.localeCompare(b.id);
};
