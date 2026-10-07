import "@tanstack/react-start/server-only";
import type { CopyObservation, EnvironmentRef, MachineId, MachineName, SwitchError, VolumeSwitchReply, VolumeSwitchRequest } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, lt, max, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { type InngestRunStatus, sendInngestEvent } from "#/modules/inngest/client";
import { createVolumeRunRequestedEvent } from "#/modules/inngest/events";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { organizationMachine } from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { observeVolumeCopies, type PloyzSdkError } from "#/modules/runtime/ployz.server";
import type { Refusal } from "#/modules/volume-run/plan";
import { volumeRun } from "#/modules/volume-run/tables";
import {
  ACTIVE_VOLUME_RUN_STATES,
  type AnsweredMember,
  type AnyVolumeRunArgs,
  dockerVolumeName,
  managementAddress,
  type Member,
  parseDockerVolumeName,
  VOLUME_RUN_MESSAGE_LIMIT,
  VOLUME_RUN_RUNNING_CHECK_MS,
  VOLUME_RUN_UNCLAIMED_LIMIT_MS,
  type VolumeRunArgs,
  type VolumeRunKind,
  type VolumeRunState,
  type VolumeRunView,
} from "#/modules/volume-run/volume-run";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";


type VolumeRunRow = typeof volumeRun.$inferSelect;

/** What every step after the claim needs from the row; plain JSON, so it survives Inngest's memo. */
export type RunContext = {
  readonly id: string;
  readonly organizationId: string;
  readonly volumeId: string;
  readonly volumeName: string;
  readonly dockerVolume: string;
  readonly kind: VolumeRunKind;
  readonly args: AnyVolumeRunArgs;
  readonly orphan: boolean;
  readonly refquotaBytes: number;
};

export type MachineRef = { readonly id: MachineId; readonly name: MachineName };

/** One request to run: the kind and its args, already shaped. */
export type VolumeRunInput = {
  [K in VolumeRunKind]: { readonly kind: K; readonly args: VolumeRunArgs[K] }
}[VolumeRunKind];

const SWITCH_STEP_SECONDS = 60;
const OBSERVE_TIMEOUT = "10 seconds";

const clip = (text: string) =>
  text.length > VOLUME_RUN_MESSAGE_LIMIT ? `${text.slice(0, VOLUME_RUN_MESSAGE_LIMIT - 1)}…` : text;

export function volumeRunView(row: VolumeRunRow): VolumeRunView {
  return {
    id: row.id,
    volume_id: row.volumeId,
    volume_name: row.volumeName,
    kind: row.kind,
    args: row.args,
    orphan: row.orphan,
    state: row.state,
    lease: row.lease,
    message: row.message,
    created_at: row.createdAt.toISOString(),
    updated_at: row.updatedAt.toISOString(),
    finished_at: row.finishedAt === null ? null : row.finishedAt.toISOString(),
  };
}

const contextOf = (row: VolumeRunRow): RunContext => ({
  id: row.id,
  organizationId: row.organizationId,
  volumeId: row.volumeId,
  volumeName: row.volumeName,
  dockerVolume: row.dockerVolume,
  kind: row.kind,
  args: row.args,
  orphan: row.orphan,
  refquotaBytes: row.refquotaBytes,
});

const ended = (state: Exclude<VolumeRunState, "requested" | "running">, message: string | null) =>
  ({ state, message: message === null ? null : clip(message), finishedAt: new Date(), updatedAt: new Date() }) as const;

export const volumeBusyMessage = (row: Pick<VolumeRunRow, "id" | "kind" | "state">) =>
  `VolumeBusy: run ${row.id} (${row.kind}) is ${row.state}; volume runs ${row.id} shows it`;

/**
 * Ask for the row's run. A row whose event could not be sent no run will claim: it ends `not_started` at once, so
 * the Volume is free for the next request.
 */
const dispatch = Effect.fn("VolumeRun.dispatch")(function* (row: VolumeRunRow) {
  const { drizzle } = yield* Database;
  yield* sendInngestEvent(createVolumeRunRequestedEvent({
    organizationId: row.organizationId,
    environment: row.environmentId,
    volumeId: row.volumeId,
    runId: row.id,
  })).pipe(
    Effect.catch(() =>
      drizzle.update(volumeRun)
        .set(ended("not_started", "the run could not be started; request it again"))
        .where(and(eq(volumeRun.id, row.id), eq(volumeRun.state, "requested"), isNull(volumeRun.inngestRunId)))),
  );
});

const readRow = Effect.fn("VolumeRun.readRow")(function* (id: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(volumeRun).where(eq(volumeRun.id, id));
  return row;
});

/** The Environment's Namespace and the Volume's config entry, from the Store. */
const resolveVolume = Effect.fn("VolumeRun.resolveVolume")(function* (
  organizationId: string,
  environment: EnvironmentRef,
  volumeId: string,
) {
  const { environment: summary, volumes } = yield* readStore(organizationId, { query: "volumes", environment });
  const volume = volumes.find((entry) => entry.id === volumeId);
  if (volume === undefined) return yield* new NotFound({ message: `No Volume ${volumeId} in ${summary.project}/${summary.name}.` });
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  const owned = namespaces.find((entry) => entry.project === summary.project && entry.environment === summary.name);
  if (owned === undefined) return yield* new NotFound({ message: `${summary.project}/${summary.name} has no Namespace yet.` });
  return {
    environmentId: summary.id,
    volumeName: volume.name,
    dockerVolume: dockerVolumeName(owned.namespace, volume.id),
    refquotaBytes: volume.storage.kind === "provisioned" ? volume.storage.maximumBytes : 0,
  };
});

/**
 * `volume mirror|sync|mirror rm`: write the run's row, then ask for its run. The partial unique index allows one
 * requested or running row per Volume; a second request is refused as VolumeBusy and names the run in the way.
 */
export const requestVolumeRun = Effect.fn("VolumeRun.request")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
) {
  const { drizzle } = yield* Database;
  const volume = yield* resolveVolume(caller.organizationId, input.environment, input.volumeId);
  const [inserted] = yield* drizzle.insert(volumeRun).values({
    organizationId: caller.organizationId,
    environmentId: volume.environmentId,
    volumeId: input.volumeId,
    volumeName: volume.volumeName,
    dockerVolume: volume.dockerVolume,
    refquotaBytes: volume.refquotaBytes,
    kind: input.kind,
    args: input.args,
    requestedByUserId: caller.userId,
  }).onConflictDoNothing().returning();
  if (inserted === undefined) {
    const [active] = yield* drizzle.select().from(volumeRun)
      .where(and(eq(volumeRun.volumeId, input.volumeId), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES)));
    const refusal: StoreRefusal = active === undefined
      ? { code: "conflict", message: "A run on this Volume just ended. Try again.", details: null }
      : { code: "conflict", message: volumeBusyMessage(active), details: { run: volumeRunView(active) } };
    return { ok: false, refusal } as const;
  }
  yield* dispatch(inserted);
  const row = yield* readRow(inserted.id);
  return { ok: true, run: volumeRunView(row ?? inserted) } as const;
});

/** `volume runs <volume>`: the Volume's runs, newest first. */
export const listVolumeRuns = Effect.fn("VolumeRun.list")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.volumeId, volumeId)))
    .orderBy(desc(volumeRun.createdAt), desc(volumeRun.id))
    .limit(50);
  return rows.map(volumeRunView);
});

/** `volume runs <volume> <run>`. */
export const getVolumeRun = Effect.fn("VolumeRun.get")(function* (organizationId: string, id: string) {
  const row = yield* readRow(id);
  if (row === undefined || row.organizationId !== organizationId) return yield* new NotFound({ message: "No such volume run." });
  return volumeRunView(row);
});

export type ClaimResult =
  | { readonly kind: "claimed"; readonly run: RunContext }
  | { readonly kind: "settled"; readonly state: VolumeRunState };

const claimedRun = (run: RunContext): ClaimResult => ({ kind: "claimed", run });

/**
 * 00-claim: the requested row becomes this run's. A row already running under this same run is returned (the claim
 * committed and the step is replayed); anything else is settled and the run does nothing.
 */
export const claimVolumeRun = Effect.fn("VolumeRun.claim")(function* (
  request: { readonly runId: string; readonly organizationId: string; readonly volumeId: string },
  inngestRunId: string,
) {
  const { drizzle } = yield* Database;
  const [claimed] = yield* drizzle.update(volumeRun)
    .set({ state: "running", inngestRunId, updatedAt: new Date() })
    .where(and(
      eq(volumeRun.id, request.runId),
      eq(volumeRun.organizationId, request.organizationId),
      eq(volumeRun.volumeId, request.volumeId),
      eq(volumeRun.state, "requested"),
    ))
    .returning();
  if (claimed !== undefined) return claimedRun(contextOf(claimed));
  const row = yield* readRow(request.runId);
  if (row === undefined || row.organizationId !== request.organizationId || row.volumeId !== request.volumeId) {
    return yield* Effect.fail(new NonRetriableError(`No volume run ${request.runId} for this Volume.`));
  }
  if (row.state === "running" && row.inngestRunId === inngestRunId) return claimedRun(contextOf(row));
  const settled: ClaimResult = { kind: "settled", state: row.state };
  return settled;
});

/** Every effect step's guard: the row is still running under this run, or the step does nothing. */
export const requireOwner = Effect.fn("VolumeRun.requireOwner")(function* (rowId: string, inngestRunId: string) {
  const { drizzle } = yield* Database;
  const [owned] = yield* drizzle.select({ id: volumeRun.id }).from(volumeRun)
    .where(and(eq(volumeRun.id, rowId), eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.state, "running")));
  if (owned === undefined) return yield* Effect.fail(new NonRetriableError(`This run no longer owns volume run ${rowId}.`));
});

/** End an owned running row; a row this run no longer owns stays as it is. */
export const endRun = Effect.fn("VolumeRun.end")(function* (
  ctx: Pick<RunContext, "id">,
  inngestRunId: string,
  state: "done" | "failed",
  message: string | null,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.update(volumeRun).set(ended(state, message))
    .where(and(eq(volumeRun.id, ctx.id), eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.state, "running")))
    .returning({ id: volumeRun.id });
  return rows.length;
});

/** Fail the row with `message`, then stop the run for good. */
export const failRun = (ctx: Pick<RunContext, "id">, inngestRunId: string, message: string) =>
  endRun(ctx, inngestRunId, "failed", message).pipe(Effect.andThen(Effect.fail(new NonRetriableError(clip(message)))));

export const refuseRun = (ctx: Pick<RunContext, "id">, inngestRunId: string, refusal: Refusal) =>
  endRun(ctx, inngestRunId, "failed", refusal.message);

const openSession = Effect.fn("VolumeRun.openSession")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return yield* Effect.fail(new Error(`The cluster is ${session.status}.`));
  return session.connected;
});

const SwitchErrorDetails = Schema.Struct({
  details: Schema.Struct({
    reason: Schema.Literals(["stale_lease", "stale_step", "expired", "precondition", "busy", "volume_switching", "no_writer", "no_capacity"]),
  }),
});
const decodeSwitchError = Schema.decodeUnknownOption(SwitchErrorDetails);

export function switchErrorOf(error: PloyzSdkError): SwitchError["reason"] | null {
  const decoded = decodeSwitchError("cause" in error ? error.cause : undefined);
  return Option.isSome(decoded) ? decoded.value.details.reason : null;
}

export type SwitchMessages = Partial<Record<SwitchError["reason"], string>>;

/** The user's words for a refusal that ends the run; undefined for one the step retries. */
export function switchFailureMessage(
  reason: SwitchError["reason"],
  ctx: Pick<RunContext, "volumeName">,
  machine: MachineRef,
  overrides: SwitchMessages = {},
): string | undefined {
  const name = ctx.volumeName;
  const override = overrides[reason];
  if (override !== undefined) return override;
  switch (reason) {
    case "stale_lease":
      return `a newer run took ${name}'s lease; this run stopped`;
    case "stale_step":
      return `${name}'s lease moved past this step`;
    case "precondition":
      return `${name}'s copy on ${machine.name} changed under this run`;
    case "volume_switching":
      return `${name} is mid-run; wait or volume release ${name}`;
    case "no_writer":
      return `${name} has no writer on ${machine.name}`;
    case "no_capacity":
      return `${machine.name} has no room for ${name}'s mirror`;
    case "busy":
    case "expired":
      return undefined;
  }
}

/**
 * One Volume Switch verb on one Machine, as one step. A refusal the run can't outlive fails the row and the run; a
 * busy Machine, an expired step or a transport error fails only this attempt, which Inngest retries.
 */
export const sendSwitch = <R extends VolumeSwitchRequest>(
  ctx: RunContext,
  inngestRunId: string,
  machine: MachineRef,
  build: (notAfterUnixSeconds: number) => R,
  overrides?: SwitchMessages,
) => {
  // SAFETY: a Machine answers each Volume Switch verb with that verb's reply; the session types the pair loosely.
  return Effect.gen(function* () {
    yield* requireOwner(ctx.id, inngestRunId);
    const session = yield* openSession(ctx.organizationId);
    const request = build(Math.floor(Date.now() / 1000) + SWITCH_STEP_SECONDS);
    return yield* session.volumeSwitch(machine.id, request).pipe(
      Effect.catch((error): Effect.Effect<never, Error, Database> => {
        const reason = switchErrorOf(error);
        const message = reason === null ? undefined : switchFailureMessage(reason, ctx, machine, overrides);
        if (message !== undefined) return failRun(ctx, inngestRunId, message);
        return Effect.fail(new Error(`${request.command} on ${machine.name}: ${error.message}`));
      }),
    );
  }).pipe(Effect.scoped) as Effect.Effect<VolumeSwitchReply<R["command"]>, Error, Database | OrganizationRuntime>;
};

/**
 * 01-observe: every runtime-frame Machine's copy of the Volume. One that does not answer in 10 s is unanswered. A
 * Docker-only Machine holds no copy and may not serve the verb, so it is left out rather than blocking the run.
 */
export const observeMembers = Effect.fn("VolumeRun.observe")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  const managed = frame.machines.filter((observed) => observed.storage?.state !== "stateless");
  return yield* Effect.forEach(managed, ({ machine, storage }) =>
    session.volumeSwitch(machine.id, { command: "inspect_volume_copy", payload: { name: ctx.dockerVolume } }).pipe(
      Effect.timeout(OBSERVE_TIMEOUT),
      Effect.option,
      Effect.map((view): Member => {
        const ref = { id: machine.id, name: machine.name };
        return Option.isSome(view)
          ? {
              machine: ref,
              address: managementAddress(machine.public_key),
              answered: true,
              pool: storage?.state === "pool" || view.value.copy !== null,
              view: view.value,
            }
          : { machine: ref, answered: false };
      }),
    ), { concurrency: "unbounded" });
}, Effect.scoped);

/**
 * 02-lease: one above every lease the answering Machines hold and every lease an earlier run on this Volume took, so
 * a Cloud database reset can't hand out a lease the Machines already moved past. Written to the row once, so a
 * replayed step adopts the same lease. Only Machines with a Pool adopt it: the verb needs one, and a Machine without
 * one holds no copy this run touches. The ones skipped are returned so the step output names them.
 */
export const takeLease = Effect.fn("VolumeRun.lease")(function* (
  ctx: RunContext,
  inngestRunId: string,
  members: readonly Member[],
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const { drizzle } = yield* Database;
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const pooled = answered.filter((member) => member.pool);
  const held = Math.max(0, ...answered.map((member) => member.view.lease?.lease ?? 0));
  const [recorded] = yield* drizzle.select({ lease: max(volumeRun.lease) }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, ctx.volumeId), sql`${volumeRun.id} <> ${ctx.id}`));
  yield* drizzle.update(volumeRun)
    .set({ lease: Math.max(held, recorded?.lease ?? 0) + 1, updatedAt: new Date() })
    .where(and(eq(volumeRun.id, ctx.id), isNull(volumeRun.lease)));
  const row = yield* readRow(ctx.id);
  const lease = row?.lease;
  if (lease === null || lease === undefined) return yield* Effect.fail(new Error(`Volume run ${ctx.id} has no lease.`));
  yield* Effect.forEach(pooled, (member) =>
    sendSwitch(ctx, inngestRunId, member.machine, (notAfter) => ({
      command: "adopt_lease",
      payload: { lease, not_after_unix_seconds: notAfter, name: ctx.dockerVolume },
    })), { concurrency: "unbounded", discard: true });
  const skipped = answered.filter((member) => !member.pool).map((member) => member.machine.name);
  return { lease, skipped };
});

const CLOSED_BY = {
  failure: { state: "failed", message: "the run failed" },
  cancellation: { state: "cancelled", message: "the run was cancelled" },
} as const;

/** A run that failed for good or was cancelled must not leave its row running. How many rows this ended. */
export const closeVolumeRun = Effect.fn("VolumeRun.close")(function* (
  inngestRunId: string,
  end: keyof typeof CLOSED_BY | { readonly error: string | null },
) {
  const { drizzle } = yield* Database;
  const closing = end === "failure" || end === "cancellation"
    ? ended(CLOSED_BY[end].state, CLOSED_BY[end].message)
    : end.error === null
      ? ended("lost", "the run ended without recording how")
      : ended("failed", end.error.trim().length > 0 ? end.error : "the run failed");
  const rows = yield* drizzle.update(volumeRun).set(closing)
    .where(and(eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.state, "running")))
    .returning({ id: volumeRun.id });
  return rows.length;
});

export type InngestRunLookup<R> = (runId: string) => Effect.Effect<InngestRunStatus, unknown, R>;

const STALE_ENDS = {
  completed: ["lost", "the run ended without recording how"],
  failed: ["failed", "the run failed"],
  cancelled: ["cancelled", "the run was cancelled"],
  missing: ["lost", "Inngest has no record of this run"],
} as const;

/**
 * The hourly sweep. A request no run claimed in 10 minutes never starts. A row running for 15 minutes without a
 * write gets its run looked up; a run Inngest ended closes the row by how it ended.
 */
export const closeStaleVolumeRuns = <R>(lookup: InngestRunLookup<R>) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const unclaimed = yield* drizzle.update(volumeRun)
    .set(ended("lost", "no run picked up this request"))
    .where(and(eq(volumeRun.state, "requested"), lt(volumeRun.createdAt, new Date(now - VOLUME_RUN_UNCLAIMED_LIMIT_MS))))
    .returning({ id: volumeRun.id });
  const running = yield* drizzle.select({ id: volumeRun.id, inngestRunId: volumeRun.inngestRunId }).from(volumeRun)
    .where(and(eq(volumeRun.state, "running"), lt(volumeRun.updatedAt, new Date(now - VOLUME_RUN_RUNNING_CHECK_MS))));
  let closed = 0;
  for (const row of running) {
    if (row.inngestRunId === null) continue;
    const status = yield* lookup(row.inngestRunId).pipe(Effect.option);
    if (Option.isNone(status) || status.value === "running") continue;
    const [state, message] = STALE_ENDS[status.value];
    const done = yield* drizzle.update(volumeRun).set(ended(state, message))
      .where(and(eq(volumeRun.id, row.id), eq(volumeRun.inngestRunId, row.inngestRunId), eq(volumeRun.state, "running")))
      .returning({ id: volumeRun.id });
    closed += done.length;
  }
  return { lost: unclaimed.length, closed };
}).pipe(Effect.withSpan("VolumeRun.closeStale"));

/**
 * Slot copies that no config entry holds and no Machine writes: what an orphan DeleteMirror run removes. A staged
 * removal not yet applied still holds its name, so its slot is not an orphan.
 */
export function orphanSlotNames(copies: CopyObservation["copies"], configNames: ReadonlySet<string>): string[] {
  const written = new Set(copies.filter((copy) => copy.role !== "slot").map((copy) => copy.name));
  const orphans = copies.filter((copy) => copy.role === "slot" && !configNames.has(copy.name) && !written.has(copy.name));
  return [...new Set(orphans.map((copy) => copy.name))].sort();
}

/** Every Docker Volume name the Organization's config entries deploy as. */
const configVolumeNames = Effect.fn("VolumeRun.configVolumeNames")(function* (organizationId: string) {
  const names = new Set<string>();
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  for (const owned of namespaces) {
    const { volumes } = yield* readStore(organizationId, {
      query: "volumes",
      environment: { project: owned.project, environment: owned.environment },
    });
    for (const volume of volumes) names.add(dockerVolumeName(owned.namespace, volume.id));
  }
  return names;
});

/** Every Volume copy on the Organization's Machines, as the SDK's copy observation reports them. */
export type CopyObserver = (organizationId: string) => Effect.Effect<CopyObservation["copies"], unknown, Database | SecretEncryption>;

export const observeOrganizationCopies: CopyObserver = (organizationId) =>
  Effect.gen(function* () {
    const loaded = yield* loadOrganizationConnections(organizationId);
    if (loaded.kind !== "ready" || loaded.connections.length === 0) return [];
    return (yield* observeVolumeCopies(loaded.connections)).copies;
  });

export const findOrphanSlots = Effect.fn("VolumeRun.findOrphanSlots")(function* (
  organizationId: string,
  observe: CopyObserver = observeOrganizationCopies,
) {
  const copies = yield* observe(organizationId);
  if (copies.length === 0) return [];
  return orphanSlotNames(copies, yield* configVolumeNames(organizationId));
});

/** Start one orphan DeleteMirror run per orphan slot name. A Volume with a run already active is skipped. */
export const startOrphanDeletes = Effect.fn("VolumeRun.startOrphanDeletes")(function* (
  organizationId: string,
  observe: CopyObserver = observeOrganizationCopies,
) {
  const { drizzle } = yield* Database;
  const names = yield* findOrphanSlots(organizationId, observe);
  const started: string[] = [];
  for (const name of names) {
    const parsed = parseDockerVolumeName(name);
    if (parsed === null) continue;
    const [row] = yield* drizzle.insert(volumeRun).values({
      organizationId,
      environmentId: parsed.namespace,
      volumeId: parsed.volumeId,
      volumeName: name,
      dockerVolume: name,
      kind: "delete_mirror",
      args: { slot: null, confirmed_name: null },
      orphan: true,
      requestedByUserId: null,
    }).onConflictDoNothing().returning();
    if (row === undefined) continue;
    yield* dispatch(row);
    started.push(row.id);
  }
  return started;
});

const NO_RUNS: readonly string[] = [];

/** Orphan cleanup that never fails its caller: a Deployment or the sweep logs the failure, and the next sweep retries. */
export const cleanOrphanSlots = (organizationId: string) =>
  startOrphanDeletes(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Orphan slot cleanup failed.", { organizationId, error }).pipe(Effect.as(NO_RUNS))),
  );

/** The Organizations with a cluster: the sweep looks for orphan slots on each. */
export const organizationsWithMachines = Effect.fn("VolumeRun.organizationsWithMachines")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinct({ organizationId: organizationMachine.organizationId }).from(organizationMachine);
  return rows.map((row) => row.organizationId);
});
