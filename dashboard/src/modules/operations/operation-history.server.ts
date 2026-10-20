import "@tanstack/react-start/server-only";
import type { MachineId, Operation, RunState, Subject } from "@ployz/sdk";
import { and, asc, desc, eq, inArray } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { drainEndMessage } from "#/modules/machines/server-drain.server";
import { machineEnrollmentToken, machineRemoveAttempt, namespaceCleanup, organizationMachine, serverDrainAttempt } from "#/modules/machines/tables";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { serviceOperations } from "#/modules/service-act/service-act.server";
import { outlivedObservation } from "#/modules/server-upgrade/server-upgrade";
import { serverUpgradeAttempt } from "#/modules/server-upgrade/tables";
import { volumeRun } from "#/modules/volume-run/tables";
import { Database } from "#/server/database.server";

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
  yielded: "superseded",
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

export const dashedAttempt = (hex: string) =>
  /^[0-9a-f]{32}$/.test(hex) ? `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}` : hex;

const isUuid = Schema.is(Uuid);

/**
 * The name of each of `machines`: as the Cluster reports it now, else as Cloud last kept it, else, for a removed Server,
 * as it last enrolled, so a missed frame or a dropped row still names it.
 */
export const serverNames = Effect.fn("OperationHistory.serverNames")(function* (organizationId: string, machines: readonly MachineId[]) {
  if (machines.length === 0) return new Map<MachineId, string>();
  const { drizzle } = yield* Database;
  const claimed = yield* drizzle.select({ machine: machineEnrollmentToken.claimedMachineId, name: machineEnrollmentToken.claimedName })
    .from(machineEnrollmentToken).where(and(
      eq(machineEnrollmentToken.organizationId, organizationId),
      inArray(machineEnrollmentToken.claimedMachineId, [...machines]),
    )).orderBy(asc(machineEnrollmentToken.claimedAt));
  const kept = yield* drizzle.select({ machine: organizationMachine.machineId, name: organizationMachine.machineName })
    .from(organizationMachine).where(and(eq(organizationMachine.organizationId, organizationId), inArray(organizationMachine.machineId, [...machines])));
  const reported = yield* firstRuntimeFrame(organizationId).pipe(
    Effect.map((frame) => (frame?.machines ?? []).map(({ machine }) => ({ machine: machine.id, name: machine.name }))),
    Effect.orElseSucceed(() => []),
  );
  return new Map([...claimed, ...kept, ...reported].flatMap(({ machine, name }) => machine === null || !name ? [] : [[machine, name] as const]));
});

const serverOperations = Effect.fn("OperationHistory.server")(function* (
  organizationId: string, machine: MachineId, attempt: string | null, limit: number, known: string | null,
) {
  const { drizzle } = yield* Database;
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
  // A row keeps the name its run started under, even on a replayed act; older rows have none, so Cloud looks it up for them.
  const looked = known || [...drains, ...removals, ...upgrades].every((row) => row.serverName !== null)
    ? undefined
    : (yield* serverNames(organizationId, [machine])).get(machine);
  const name = (row: { serverName: string | null }) => row.serverName || known || looked || machine;
  return [
    ...drains.map((row) => operation({
      attempt: row.id, run: `drain ${name(row)}`, state: row.state, requestedAt: row.requestedAt, endedAt: row.endedAt, reason: drainEndMessage(row),
    })),
    ...removals.map((row) => operation({
      attempt: row.id, run: `remove ${name(row)}`, state: row.state, requestedAt: row.createdAt, endedAt: row.terminalAt, reason: row.failureMessage,
    })),
    ...upgrades.map((row) => operation({
      attempt: dashedAttempt(row.attemptId),
      run: `upgrade ${name(row)}`,
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

/** `serverName`, the name an act already resolved, names a Server run before any name Cloud looks up. */
export const operationsOn = (organizationId: string, subject: Subject, attempt: string | null, limit: number, serverName: string | null = null) => {
  switch (subject.kind) {
    case "server":
      return serverOperations(organizationId, subject.machine, attempt, limit, serverName);
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
