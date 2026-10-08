import "@tanstack/react-start/server-only";
import type { ContainerId, CopyObservation, EnvironmentRef, MachineId, MachineName, SwitchError, VolumeSwitchReply, VolumeSwitchRequest } from "@ployz/sdk";
import { and, desc, eq, inArray, isNotNull, isNull, lt, max, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { type InngestRunStatus, sendInngestEvent } from "#/modules/inngest/client";
import { createVolumeRunRequestedEvent } from "#/modules/inngest/events";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { machineRemoveAttempt, organizationMachine } from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { observeVolumeCopies, type PloyzSdkError, sdkFailureMessage } from "#/modules/runtime/ployz.server";
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
  type ServiceSpec,
  VOLUME_RUN_MESSAGE_LIMIT,
  VOLUME_RUN_RUNNING_CHECK_MS,
  VOLUME_RUN_UNCLAIMED_LIMIT_MS,
  type VolumeRunInput,
  type VolumeRunKind,
  type VolumeRunState,
  type VolumeRunView,
} from "#/modules/volume-run/volume-run";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

type VolumeRunRow = typeof volumeRun.$inferSelect;

/** Plain JSON: Inngest memoizes step output as JSON. */
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

export const listVolumeRuns = Effect.fn("VolumeRun.list")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.volumeId, volumeId)))
    .orderBy(desc(volumeRun.createdAt), desc(volumeRun.id))
    .limit(50);
  return rows.map(volumeRunView);
});

export const getVolumeRun = Effect.fn("VolumeRun.get")(function* (organizationId: string, id: string) {
  const row = yield* readRow(id);
  if (row === undefined || row.organizationId !== organizationId) return yield* new NotFound({ message: "No such volume run." });
  return volumeRunView(row);
});

export type ClaimResult =
  | { readonly kind: "claimed"; readonly run: RunContext }
  | { readonly kind: "settled"; readonly state: VolumeRunState };

const claimedRun = (run: RunContext): ClaimResult => ({ kind: "claimed", run });

/** Inngest can rerun a step whose write committed, so a row already running under this run counts as claimed. */
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

export const requireOwner = Effect.fn("VolumeRun.requireOwner")(function* (rowId: string, inngestRunId: string) {
  const { drizzle } = yield* Database;
  const [owned] = yield* drizzle.select({ id: volumeRun.id }).from(volumeRun)
    .where(and(eq(volumeRun.id, rowId), eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.state, "running")));
  if (owned === undefined) return yield* Effect.fail(new NonRetriableError(`This run no longer owns volume run ${rowId}.`));
});

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

/** A failed attempt the step may retry; `reason` is the Machine's SwitchError, if it gave one. */
export class SwitchAttemptError extends Error {
  constructor(readonly reason: SwitchError["reason"] | null, message: string) {
    super(message);
  }
}

/**
 * With `throw` a refusal fails only the step, so a Move can thaw its source or add its next command before it records
 * the failure. A newer lease or a later step still ends the run, since nothing of this run may follow it.
 */
export type SwitchOptions = { readonly messages?: SwitchMessages; readonly onRefusal?: "fail" | "throw" };

export const sendSwitch = <R extends VolumeSwitchRequest>(
  ctx: RunContext,
  inngestRunId: string,
  machine: MachineRef,
  build: (notAfterUnixSeconds: number) => R,
  options: SwitchOptions = {},
) => {
  // SAFETY: a Machine answers each Volume Switch verb with that verb's reply; the session types the pair loosely.
  return Effect.gen(function* () {
    yield* requireOwner(ctx.id, inngestRunId);
    const session = yield* openSession(ctx.organizationId);
    const request = build(Math.floor(Date.now() / 1000) + SWITCH_STEP_SECONDS);
    return yield* session.volumeSwitch(machine.id, request).pipe(
      Effect.catch((error): Effect.Effect<never, Error, Database> => {
        const reason = switchErrorOf(error);
        const message = reason === null ? undefined : switchFailureMessage(reason, ctx, machine, options.messages);
        if (message !== undefined) {
          const thrown = options.onRefusal === "throw" && reason !== "stale_lease" && reason !== "stale_step";
          return thrown ? Effect.fail(new NonRetriableError(clip(message))) : failRun(ctx, inngestRunId, message);
        }
        return Effect.fail(new SwitchAttemptError(reason, `${request.command} on ${machine.name}: ${sdkFailureMessage(error)}`));
      }),
    );
  }).pipe(Effect.scoped) as Effect.Effect<VolumeSwitchReply<R["command"]>, Error, Database | OrganizationRuntime>;
};

export type Holder = ServiceSpec & { readonly containerId: ContainerId };

/** The one Service container on the writer that mounts this Volume: Freeze stops it, Thaw restarts it, and Start rebuilds it on the target. */
export const findHolder = Effect.fn("VolumeRun.findHolder")(function* (ctx: RunContext, inngestRunId: string, writer: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  const holders = frame.containers.filter((container) =>
    container.machine_id === writer.id
    && container.kind === "service_container"
    && container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === ctx.dockerVolume));
  const [holder] = holders;
  if (holder === undefined || holders.length > 1) {
    const found = holders.length === 0 ? "no Service container" : `${holders.length} Service containers`;
    return { ok: false, refusal: { code: "invalid", message: `${ctx.volumeName} has ${found} on ${writer.name}; a move needs exactly one` } } as const;
  }
  return { ok: true, holder: { containerId: holder.container_id, namespace: holder.namespace, redactedSpec: holder.resolved_spec } } as const;
}, Effect.scoped);

/** Restore registers the Volume with the spec of the newest Service container that mounts it, stopped ones included. */
export const findSpec = Effect.fn("VolumeRun.findSpec")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  const newest = frame.containers
    .filter((container) =>
      container.kind === "service_container"
      && container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === ctx.dockerVolume))
    .reduce<(typeof frame.containers)[number] | undefined>(
      (found, container) => found === undefined || container.created_at_unix_nanos > found.created_at_unix_nanos ? container : found,
      undefined,
    );
  if (newest !== undefined) return { ok: true, spec: { namespace: newest.namespace, redactedSpec: newest.resolved_spec } satisfies ServiceSpec } as const;
  const { drizzle } = yield* Database;
  const [kept] = yield* drizzle.select({ spec: volumeRun.serviceSpec }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, ctx.volumeId), isNotNull(volumeRun.serviceSpec)))
    .orderBy(desc(volumeRun.createdAt))
    .limit(1);
  if (kept?.spec === null || kept?.spec === undefined) {
    return { ok: false, refusal: { code: "invalid", message: `no Service container mounts ${ctx.volumeName}, and no earlier run saw one; Restore takes its spec from one` } } as const;
  }
  return { ok: true, spec: kept.spec } as const;
}, Effect.scoped);

/** A removed Server's last answer came before its removal; past that plus the grace and the Machine task budget, its Promote or Start can no longer run. */
export const DEPARTED_GRACE_MS = 60_000;
export const MACHINE_TASK_BUDGET_MS = 10 * 60_000;

/**
 * Cloud can't see the copies of a Server it no longer reaches, so Restore waits out every `--no-reset` removal in the
 * Organization: the deadline is the latest one's end plus the grace and the task budget, or null once it has passed.
 */
export const departedDeadline = Effect.fn("VolumeRun.departedDeadline")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  const { drizzle } = yield* Database;
  const [latest] = yield* drizzle.select({ at: max(machineRemoveAttempt.terminalAt) }).from(machineRemoveAttempt)
    .where(and(
      eq(machineRemoveAttempt.organizationId, ctx.organizationId),
      eq(machineRemoveAttempt.noReset, true),
      eq(machineRemoveAttempt.state, "succeeded"),
    ));
  if (latest?.at === null || latest?.at === undefined) return null;
  const deadline = latest.at.getTime() + DEPARTED_GRACE_MS + MACHINE_TASK_BUDGET_MS;
  return deadline > Date.now() ? new Date(deadline).toISOString() : null;
});

/** Why `server rm` must wait: an open run names this Machine. Null when none does. */
export const openRunNaming = Effect.fn("VolumeRun.openRunNaming")(function* (organizationId: string, machineId: MachineId) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ volumeName: volumeRun.volumeName, kind: volumeRun.kind }).from(volumeRun)
    .where(and(
      eq(volumeRun.organizationId, organizationId),
      inArray(volumeRun.state, [...ACTIVE_VOLUME_RUN_STATES]),
      sql`${volumeRun.machineIds} @> array[${machineId}]::text[]`,
    ))
    .limit(1);
  if (row === undefined) return null;
  const label = row.kind === "delete_mirror" ? "mirror removal" : row.kind;
  const release = row.kind === "move" ? `, or volume release ${row.volumeName}` : "";
  return `${row.volumeName}'s ${label} uses this Server; wait for it to end${release}`;
});

export const copyImage = Effect.fn("VolumeRun.copyImage")(function* (ctx: RunContext, inngestRunId: string, from: MachineRef, holder: Holder, to: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  yield* session.copyContainerImage(from.id, holder.containerId, to.id).pipe(
    Effect.mapError((error) => new Error(`copying ${holder.redactedSpec.name}'s image to ${to.name}: ${sdkFailureMessage(error)}`)),
  );
}, Effect.scoped);

type StartHanded = Extract<VolumeSwitchRequest, { command: "start_handed_container" }>;

export const startHanded = Effect.fn("VolumeRun.startHanded")(function* (
  ctx: RunContext,
  inngestRunId: string,
  from: MachineRef,
  holder: Holder,
  to: MachineRef,
  stamp: (notAfter: number) => StartHanded["payload"]["switch"],
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const { container } = yield* session.inspectContainer(from.id, holder.containerId).pipe(
    Effect.mapError((error) => new Error(`reading ${holder.redactedSpec.name}'s spec on ${from.name}: ${sdkFailureMessage(error)}`)),
  );
  const spec = container.resolved_spec;
  yield* sendSwitch(ctx, inngestRunId, to, (notAfter): StartHanded => ({
    command: "start_handed_container",
    payload: {
      switch: stamp(notAfter),
      name: ctx.dockerVolume,
      namespace: holder.namespace,
      resolved_spec: { ...spec, container: { ...spec.container, pull_policy: "never" } },
    },
  }), { onRefusal: "throw" });
}, Effect.scoped);

export const observeMembers = Effect.fn("VolumeRun.observe")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  // A Docker-only Machine holds no copy and may not serve the verb, so it must not block the run.
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

/** Above every lease the Machines hold and every lease this Volume's runs took, so a Cloud database reset can't reuse one. */
export const takeLease = Effect.fn("VolumeRun.lease")(function* (
  ctx: RunContext,
  inngestRunId: string,
  members: readonly Member[],
  participants: readonly MachineId[],
  spec: ServiceSpec | null,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const { drizzle } = yield* Database;
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const pooled = answered.filter((member) => member.pool);
  const held = Math.max(0, ...answered.map((member) => member.view.lease?.lease ?? 0));
  const [recorded] = yield* drizzle.select({ lease: max(volumeRun.lease) }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, ctx.volumeId), sql`${volumeRun.id} <> ${ctx.id}`));
  yield* drizzle.update(volumeRun)
    .set({ lease: Math.max(held, recorded?.lease ?? 0) + 1, machineIds: [...participants], serviceSpec: spec ?? undefined, updatedAt: new Date() })
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

/** A staged removal not yet applied still holds its name, so its slot is not an orphan. */
export function orphanSlotNames(copies: CopyObservation["copies"], configNames: ReadonlySet<string>): string[] {
  const written = new Set(copies.filter((copy) => copy.role !== "slot").map((copy) => copy.name));
  const orphans = copies.filter((copy) => copy.role === "slot" && !configNames.has(copy.name) && !written.has(copy.name));
  return [...new Set(orphans.map((copy) => copy.name))].sort();
}

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

export const cleanOrphanSlots = (organizationId: string) =>
  startOrphanDeletes(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Orphan slot cleanup failed.", { organizationId, error }).pipe(Effect.as(NO_RUNS))),
  );

export const organizationsWithMachines = Effect.fn("VolumeRun.organizationsWithMachines")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinct({ organizationId: organizationMachine.organizationId }).from(organizationMachine);
  return rows.map((row) => row.organizationId);
});
