import "@tanstack/react-start/server-only";
import type { MachineId, Operation, RunState, Subject } from "@ployz/sdk";
import { and, desc, eq } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { machineRemoveAttempt, namespaceCleanup, serverDrainAttempt } from "#/modules/machines/tables";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { serviceOperations } from "#/modules/service-act/service-act.server";
import { outlivedObservation } from "#/modules/server-upgrade/server-upgrade";
import { serverUpgradeAttempt } from "#/modules/server-upgrade/tables";
import { volumeRun } from "#/modules/volume-run/tables";
import { Database } from "#/server/database.server";

/** Every attempt table's state, as the run it reads as. */
const RUN_STATES = {
  pending: "accepted",
  requested: "accepted",
  running: "running",
  finished: "done",
  succeeded: "done",
  done: "done",
  failed: "failed",
  cancelled: "failed",
  not_started: "failed",
  missing_identities: "failed",
  unknown: "unknown",
  lost: "unknown",
  interrupted: "unknown",
} as const satisfies Record<string, RunState>;

const seconds = (at: Date) => Math.floor(at.getTime() / 1000);

const operation = (row: { attempt: string; run: string; state: keyof typeof RUN_STATES; requestedAt: Date; endedAt: Date | null; reason: string | null }): Operation => ({
  attempt: row.attempt,
  run: row.run,
  state: RUN_STATES[row.state],
  requested_at: seconds(row.requestedAt),
  ended_at: row.endedAt === null ? null : seconds(row.endedAt),
  reason: row.reason,
  servers: [],
  kept: [],
  unanswered: [],
});

/** An Upgrade's attempt is 32 hex digits; the agent reads it as a UUID like every other attempt. */
export const dashedAttempt = (hex: string) =>
  /^[0-9a-f]{32}$/.test(hex) ? `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}` : hex;

const isUuid = Schema.is(Uuid);

/** Server names by machine ID, best effort: a Cluster that doesn't answer leaves the IDs. */
export const serverNames = (organizationId: string) =>
  firstRuntimeFrame(organizationId).pipe(
    Effect.map((frame) => new Map((frame?.machines ?? []).map(({ machine }) => [machine.id, machine.name]))),
    Effect.orElseSucceed(() => new Map()),
  );

const serverOperations = Effect.fn("OperationHistory.server")(function* (organizationId: string, machine: MachineId, attempt: string | null, limit: number) {
  const { drizzle } = yield* Database;
  const name = (yield* serverNames(organizationId)).get(machine) ?? machine;
  const uuid = attempt !== null && isUuid(attempt) ? attempt : null;
  const hex = attempt?.replaceAll("-", "").toLowerCase() ?? null;
  const drains = attempt !== null && uuid === null ? [] : yield* drizzle.select().from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, organizationId),
    eq(serverDrainAttempt.machineId, machine),
    uuid === null ? undefined : eq(serverDrainAttempt.id, uuid),
  )).orderBy(desc(serverDrainAttempt.requestedAt)).limit(limit);
  const removals = attempt !== null && uuid === null ? [] : yield* drizzle.select().from(machineRemoveAttempt).where(and(
    eq(machineRemoveAttempt.organizationId, organizationId),
    eq(machineRemoveAttempt.machineId, machine),
    uuid === null ? undefined : eq(machineRemoveAttempt.id, uuid),
  )).orderBy(desc(machineRemoveAttempt.createdAt)).limit(limit);
  const upgrades = yield* drizzle.select().from(serverUpgradeAttempt).where(and(
    eq(serverUpgradeAttempt.organizationId, organizationId),
    eq(serverUpgradeAttempt.machineId, machine),
    hex === null ? undefined : eq(serverUpgradeAttempt.attemptId, hex),
  )).orderBy(desc(serverUpgradeAttempt.startedAt)).limit(limit);
  const now = Date.now();
  return [
    ...drains.map((row) => operation({
      attempt: row.id, run: `drain ${name}`, state: row.state, requestedAt: row.requestedAt, endedAt: row.endedAt, reason: row.refusalMessage,
    })),
    ...removals.map((row) => operation({
      attempt: row.id, run: `remove ${name}`, state: row.state, requestedAt: row.createdAt, endedAt: row.terminalAt, reason: row.failureMessage,
    })),
    ...upgrades.map((row) => operation({
      attempt: dashedAttempt(row.attemptId),
      run: `upgrade ${name}`,
      state: row.outcome === "running" && outlivedObservation(row.startedAt, now) ? "unknown" : row.outcome,
      requestedAt: row.startedAt,
      endedAt: row.endedAt,
      reason: row.error,
    })),
  ].sort((a, b) => b.requested_at - a.requested_at).slice(0, limit);
});

const volumeOperations = Effect.fn("OperationHistory.volume")(function* (organizationId: string, volume: string, attempt: string | null, limit: number) {
  if (attempt !== null && !isUuid(attempt)) return [];
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun).where(and(
    eq(volumeRun.organizationId, organizationId),
    eq(volumeRun.volumeId, volume),
    attempt === null ? undefined : eq(volumeRun.id, attempt),
  )).orderBy(desc(volumeRun.createdAt)).limit(limit);
  return rows.map((row) => operation({
    attempt: row.id,
    run: `${row.kind.replace("_", " ")} ${row.volumeName}`,
    state: row.state,
    requestedAt: row.createdAt,
    endedAt: row.finishedAt,
    reason: row.message,
  }));
});

const leftoverOperations = Effect.fn("OperationHistory.leftover")(function* (organizationId: string, namespace: string, attempt: string | null, limit: number) {
  if (attempt !== null && !isUuid(attempt)) return [];
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(namespaceCleanup).where(and(
    eq(namespaceCleanup.organizationId, organizationId),
    eq(namespaceCleanup.namespace, namespace),
    attempt === null ? undefined : eq(namespaceCleanup.id, attempt),
  )).orderBy(desc(namespaceCleanup.requestedAt)).limit(limit);
  return rows.map((row) => operation({
    attempt: row.id, run: `clean ${namespace}`, state: row.state, requestedAt: row.requestedAt, endedAt: row.endedAt, reason: row.endMessage,
  }));
});

/** The runs on one subject, newest first, or the one `attempt` names. */
export const operationsOn = (organizationId: string, subject: Subject, attempt: string | null, limit: number) => {
  switch (subject.kind) {
    case "server":
      return serverOperations(organizationId, subject.machine, attempt, limit);
    case "service":
      return attempt !== null && !isUuid(attempt)
        ? Effect.succeed<Operation[]>([])
        : serviceOperations(organizationId, { environment: subject.environment, service: subject.service }, attempt, limit);
    case "volume":
      return volumeOperations(organizationId, subject.volume, attempt, limit);
    case "leftover":
      return leftoverOperations(organizationId, subject.namespace, attempt, limit);
  }
};
