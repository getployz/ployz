import "@tanstack/react-start/server-only";
import type { ContainerId, ContainerObservation, CopyObservation, DestructiveEffect, EnvironmentRef, MachineId, MachineName, SnapshotRefusal, SwitchError, VolumeSwitchReply, VolumeSwitchRequest } from "@ployz/sdk";
import { and, desc, eq, inArray, isNotNull, isNull, lt, max, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import type { OperationVerb } from "#/modules/approvals/approvals";
import { readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { createVolumeRunRequestedEvent } from "#/modules/inngest/events";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { organizationMachine } from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { type Requirement, requirementFor, requirements } from "#/modules/runtime/daemon-compat";
import { observeVolumeCopies, type PloyzSdkError, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { type Planned, planFromCopies, type Refusal, type RefusalCode } from "#/modules/volume-run/plan";
import { isBehind, isCurrent, releaseLine } from "#/modules/server-upgrade/server-upgrade";
import { channelRelease, organizationReleaseChannel } from "#/modules/server-upgrade/server-upgrade.server";
import { volumeRun } from "#/modules/volume-run/tables";
import {
  ACTIVE_VOLUME_RUN_STATES,
  type AnsweredMember,
  type AnyVolumeRunArgs,
  copyName,
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
type PlannedPhase = Extract<Planned, { ok: true }>["phase"];

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

const enqueueRun = (row: VolumeRunRow) =>
  enqueueEvent(createVolumeRunRequestedEvent({
    organizationId: row.organizationId,
    environment: row.environmentId,
    volumeId: row.volumeId,
    runId: row.id,
  }));

const readRow = Effect.fn("VolumeRun.readRow")(function* (id: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(volumeRun).where(eq(volumeRun.id, id));
  return row;
});

type ResolvedVolume = Effect.Success<ReturnType<typeof resolveVolume>>;

export const resolveVolume = Effect.fn("VolumeRun.resolveVolume")(function* (
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

/** A seed copies the source's dataset as is, so the restored Volume must still ask for the source's storage. */
const seedLandingRefusal = Effect.fn("VolumeRun.seedLandingRefusal")(function* (
  organizationId: string,
  environment: EnvironmentRef,
  sourceId: string,
  intoId: string,
) {
  const { volumes } = yield* readStore(organizationId, { query: "volumes", environment });
  const source = volumes.find((entry) => entry.id === sourceId);
  const into = volumes.find((entry) => entry.id === intoId);
  if (source === undefined || into === undefined) {
    return { code: "conflict", message: "The restored Volume or its source is gone. Discard the restore.", details: null } satisfies StoreRefusal;
  }
  if (JSON.stringify(source.storage) === JSON.stringify(into.storage)) return null;
  return {
    code: "conflict",
    message: `${into.name}'s storage no longer matches ${source.name}'s, so the restore can't land on it. Discard the restore and restore again.`,
    details: null,
  } satisfies StoreRefusal;
});

const refusalCode = {
  unanswered: "unavailable",
  confirm_required: "confirmation_required",
  invalid: "invalid_argument",
  no_pool: "invalid_argument",
  no_writer: "conflict",
  two_writers: "conflict",
  volume_switching: "conflict",
  handed: "conflict",
  stale_slot: "conflict",
  second_mirror: "conflict",
  no_mirror: "conflict",
  no_copy: "conflict",
  another_copy: "conflict",
  server_lacks: "unsupported",
} satisfies Record<RefusalCode, string>;

const deletesCopy = (volume: ResolvedVolume, member: AnsweredMember): DestructiveEffect => ({
  kind: "deletes_volume",
  name: copyName(volume.volumeName, member.machine.name),
  node: `${member.machine.id}/${volume.dockerVolume}`,
  path: `volumes/${member.machine.id}/${volume.dockerVolume}`,
});

/** The copies a phase deletes with no other copy left: a Mirror Remove with no writer, or a Release that discards the handed copy. */
function phaseEffects(volume: ResolvedVolume, phase: PlannedPhase): DestructiveEffect[] {
  switch (phase.kind) {
    case "delete_mirror":
      return phase.forget === null ? phase.destroy.map((member) => deletesCopy(volume, member)) : [];
    case "release":
      return phase.thaw && phase.mirror !== null ? [deletesCopy(volume, phase.mirror)] : [];
    case "mirror":
    case "sync":
    case "move":
    case "restore":
    case "demote":
    case "seed":
    case "restore_snapshot":
      return [];
  }
}

const VERBS = {
  release: "release",
  delete_mirror: "remove_mirror",
  mirror: null,
  sync: null,
  move: null,
  restore: null,
  seed: null,
  restore_snapshot: null,
} satisfies Record<VolumeRunKind, OperationVerb | null>;

export type VolumeRunPlan =
  | { readonly ok: true; readonly volume: ResolvedVolume; readonly asked: OperationAsked | null }
  | { readonly ok: false; readonly refusal: StoreRefusal };

/**
 * A requested run planned against the copies the Servers hold now, so a refusal reaches the caller before any row
 * exists. The run plans again when it starts, since the copies may move in between.
 */
export const planVolumeRun = Effect.fn("VolumeRun.plan")(function* (
  organizationId: string,
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
) {
  const volume = yield* resolveVolume(organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal } satisfies VolumeRunPlan;
  }
  const members = yield* observeVolumeMembers(organizationId, volume.dockerVolume, input.kind).pipe(Effect.option);
  if (Option.isNone(members)) {
    return { ok: false, refusal: { code: "unavailable", message: "Your servers aren't answering. Try again once they are.", details: null } } satisfies VolumeRunPlan;
  }
  const planned = planFromCopies({ ...input, volumeName: volume.volumeName, orphan: false }, members.value);
  if (!planned.ok) {
    const { code, message, to } = planned.refusal;
    const details = to === undefined ? null : { move_to: to };
    return { ok: false, refusal: { code: refusalCode[code], message, details } } satisfies VolumeRunPlan;
  }
  const verb = VERBS[input.kind];
  const effects = phaseEffects(volume, planned.phase);
  const asked: OperationAsked | null = verb === null ? null : {
    subject: `volume:${input.volumeId}`,
    verb,
    name: volume.volumeName,
    preview: { volume: input.volumeId, copies: effects.map((effect) => effect.node) },
    effects,
  };
  return { ok: true, volume, asked } satisfies VolumeRunPlan;
});

export const requestVolumeRun = Effect.fn("VolumeRun.request")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
  id: string = crypto.randomUUID(),
) {
  const database = yield* Database;
  const volume = yield* resolveVolume(caller.organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(caller.organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal } as const;
  }
  return yield* database.transaction(insertRun(caller, input, volume, id));
});

/** Writes the run under `id` with its outbox event, or refuses with the run already active on the Volume. */
export const insertRun = Effect.fn("VolumeRun.insert")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly volumeId: string } & VolumeRunInput,
  volume: ResolvedVolume,
  id: string,
) {
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(volumeRun).values({
    id,
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
  yield* enqueueRun(inserted);
  return { ok: true, run: volumeRunView(inserted) } as const;
});

export const listVolumeRuns = Effect.fn("VolumeRun.list")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.volumeId, volumeId)))
    .orderBy(desc(volumeRun.createdAt), desc(volumeRun.id))
    .limit(50);
  return rows.map(volumeRunView);
});

/** The Volume's copies as each managed Machine answers for them, for the panel to plan its offers from. */
export const inspectVolumeCopies = Effect.fn("VolumeRun.inspectCopies")(function* (organizationId: string, environment: EnvironmentRef, volumeId: string) {
  const volume = yield* resolveVolume(organizationId, environment, volumeId);
  return yield* observeVolumeMembers(organizationId, volume.dockerVolume);
});

/** Every run in progress across the organization's Volumes, for the canvas trays. */
export const listActiveVolumeRuns = Effect.fn("VolumeRun.listActive")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES)));
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
    reason: Schema.Literals(["stale_lease", "stale_step", "precondition", "busy", "volume_switching", "no_writer", "no_capacity", "restore_point_gone"]),
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
    case "restore_point_gone":
      return `the restore point is gone from ${name} on ${machine.name}`;
    case "busy":
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
  request: R,
  options: SwitchOptions = {},
) => {
  // SAFETY: a Machine answers each Volume Switch verb with that verb's reply; the session types the pair loosely.
  return Effect.gen(function* () {
    yield* requireOwner(ctx.id, inngestRunId);
    const session = yield* openSession(ctx.organizationId);
    return yield* session.volumeSwitch(machine.id, request).pipe(
      Effect.catch((error): Effect.Effect<never, Error, Database> => {
        const reason = switchErrorOf(error);
        const message = reason === null ? undefined : switchFailureMessage(reason, ctx, machine, options.messages);
        if (message !== undefined) {
          const thrown = options.onRefusal === "throw" && reason !== "stale_lease" && reason !== "stale_step";
          return thrown ? Effect.fail(new NonRetriableError(clip(message))) : failRun(ctx, inngestRunId, message);
        }
        return Effect.fail(new SwitchAttemptError(reason, `${request.command} on ${machine.name}: ${sdkFailureMessage(error, [machine])}`));
      }),
    );
  }).pipe(Effect.scoped) as Effect.Effect<VolumeSwitchReply<R["command"]>, Error, Database | OrganizationRuntime>;
};

export type Holder = ServiceSpec & { readonly containerId: ContainerId };

const holdersOn = (ctx: RunContext, containers: readonly ContainerObservation[], writer: MachineRef) =>
  containers.filter((container) =>
    container.machine_id === writer.id
    && container.kind === "service_container"
    && container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === ctx.dockerVolume));

/** The one Service container on the writer that mounts this Volume: Freeze stops it, Thaw restarts it, and Start rebuilds it on the target. */
export const findHolder = Effect.fn("VolumeRun.findHolder")(function* (ctx: RunContext, inngestRunId: string, writer: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  const holders = holdersOn(ctx, frame.containers, writer);
  const [holder] = holders;
  if (holder === undefined || holders.length > 1) {
    const found = holders.length === 0 ? "no Service container" : `${holders.length} Service containers`;
    return { ok: false, refusal: { code: "invalid", message: `${ctx.volumeName} has ${found} on ${writer.name}; a move needs exactly one` } } as const;
  }
  return { ok: true, holder: { containerId: holder.container_id, namespace: holder.namespace, redactedSpec: holder.resolved_spec } } as const;
}, Effect.scoped);

/** The Service containers running on the writer with this Volume mounted, which a snapshot restore stops for its swap. */
export const runningHolders = Effect.fn("VolumeRun.runningHolders")(function* (ctx: RunContext, inngestRunId: string, writer: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  return holdersOn(ctx, frame.containers, writer)
    .filter((container) => container.runtime.state === "running")
    .map((container) => container.container_id);
}, Effect.scoped);

export const holderVerb = Effect.fn("VolumeRun.holderVerb")(function* (
  ctx: RunContext,
  inngestRunId: string,
  writer: MachineRef,
  container: ContainerId,
  verb: "stop" | "start",
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const sent = verb === "stop" ? session.stopContainer(writer.id, container).pipe(Effect.as("stopped" as const)) : session.startContainer(writer.id, container);
  return yield* sent.pipe(Effect.mapError((error) => new Error(`${verb} ${container} on ${writer.name}: ${sdkFailureMessage(error, [writer])}`)));
}, Effect.scoped);

const decodeSnapshotRefusal = Schema.decodeUnknownOption(Schema.Struct({
  details: Schema.Struct({ reason: Schema.Literals(["mismatch", "no_writer", "busy", "not_found"]) }),
}));

export function snapshotRefusalOf(error: PloyzSdkError): SnapshotRefusal | null {
  const decoded = decodeSnapshotRefusal("cause" in error ? error.cause : undefined);
  return Option.isSome(decoded) ? decoded.value.details.reason : null;
}

type SetSnapshot = Extract<VolumeSwitchRequest, { command: "set_volume_snapshot" }>;

/**
 * Busy is a SwitchAttemptError the caller waits out; any other refusal is a NonRetriableError, so the caller can start
 * the Service again before it fails the run. A Mismatch never retries: the writer moved since the snapshot was listed,
 * and only a fresh listing names the right epoch.
 */
export const setSnapshot = (ctx: RunContext, inngestRunId: string, writer: MachineRef, payload: SetSnapshot["payload"]) =>
  Effect.gen(function* () {
    yield* requireOwner(ctx.id, inngestRunId);
    const session = yield* openSession(ctx.organizationId);
    return yield* session.volumeSwitch(writer.id, { command: "set_volume_snapshot", payload } satisfies SetSnapshot).pipe(
      Effect.asVoid,
      Effect.catch((error): Effect.Effect<never, Error> => {
        const reason = snapshotRefusalOf(error);
        switch (reason) {
          case "busy":
            return Effect.fail(new SwitchAttemptError("busy", `${ctx.volumeName} is busy on ${writer.name}`));
          case "mismatch":
            return Effect.fail(new NonRetriableError(`${ctx.volumeName} moved since its snapshots were listed; restore again`));
          case "no_writer":
            return Effect.fail(new NonRetriableError(`${ctx.volumeName} has no writer on ${writer.name}`));
          case "not_found":
            return Effect.fail(new NonRetriableError("Snapshot not found"));
          case null:
            return Effect.fail(new SwitchAttemptError(null, `set_volume_snapshot on ${writer.name}: ${sdkFailureMessage(error, [writer])}`));
        }
      }),
    );
  }).pipe(Effect.scoped);

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

export const copyImage = Effect.fn("VolumeRun.copyImage")(function* (ctx: RunContext, inngestRunId: string, from: MachineRef, holder: Holder, to: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  yield* session.copyContainerImage(from.id, holder.containerId, to.id).pipe(
    Effect.mapError((error) => new Error(`copying ${holder.redactedSpec.name}'s image to ${to.name}: ${sdkFailureMessage(error, [from, to])}`)),
  );
}, Effect.scoped);

type StartHanded = Extract<VolumeSwitchRequest, { command: "start_handed_container" }>;

export const startHanded = Effect.fn("VolumeRun.startHanded")(function* (
  ctx: RunContext,
  inngestRunId: string,
  from: MachineRef,
  holder: Holder,
  to: MachineRef,
  stamp: StartHanded["payload"]["switch"],
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const { container } = yield* session.inspectContainer(from.id, holder.containerId).pipe(
    Effect.mapError((error) => new Error(`reading ${holder.redactedSpec.name}'s spec on ${from.name}: ${sdkFailureMessage(error, [from])}`)),
  );
  const spec = container.resolved_spec;
  yield* sendSwitch(ctx, inngestRunId, to, {
    command: "start_handed_container",
    payload: {
      switch: stamp,
      name: ctx.dockerVolume,
      namespace: holder.namespace,
      resolved_spec: { ...spec, container: { ...spec.container, pull_policy: "never" } },
    },
  } satisfies StartHanded, { onRefusal: "throw" });
}, Effect.scoped);

/** Each managed Server's copy of `dockerVolume`; a Server that doesn't answer in time is a member that didn't answer. */
export const observeVolumeMembers = Effect.fn("VolumeRun.observeMembers")(function* (organizationId: string, dockerVolume: string, kind?: VolumeRunKind) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  // A Docker-only Machine holds no copy and may not serve the verb, so it must not block the run.
  const managed = frame.machines.filter((observed) => observed.storage?.state !== "stateless");
  return yield* Effect.forEach(managed, ({ machine, storage }) =>
    session.volumeSwitch(machine.id, { command: "inspect_volume_copy", payload: { name: dockerVolume } }).pipe(
      Effect.map((view): Member => ({
        machine: { id: machine.id, name: machine.name },
        address: managementAddress(machine.public_key),
        answered: true,
        pool: storage?.state === "pool" || view.copy !== null,
        view,
      })),
      Effect.timeout(OBSERVE_TIMEOUT),
      Effect.catchTag("ServerLacks", (lack) => lackSentence(organizationId, lack.capability, { name: machine.name, daemonVersion: machine.runtime.daemon_version }, kind).pipe(
        Effect.map((lack): Member => ({ machine: { id: machine.id, name: machine.name }, answered: false, lack })),
      )),
      Effect.orElseSucceed((): Member => ({ machine: { id: machine.id, name: machine.name }, answered: false })),
    ), { concurrency: "unbounded" });
}, Effect.scoped);

/** What a run of each kind is called when a Server can't run it. */
const RUN_FEATURES = {
  mirror: "Volume mirror", sync: "Mirror refresh", delete_mirror: "Mirror removal", move: "Volume move", release: "Volume move", restore: "Using a mirror", seed: "Volume restore",
  restore_snapshot: "Snapshot restore",
} satisfies Record<VolumeRunKind, string>;

const lackSentence = Effect.fn("VolumeRun.lackSentence")(function* (
  organizationId: string, capability: string, server: { readonly name: string; readonly daemonVersion: string }, kind: VolumeRunKind | undefined,
) {
  const required = requirementFor(capability) ?? requirements.volumeMove;
  const feature: Requirement = kind === undefined ? required : { capability: required.capability, feature: RUN_FEATURES[kind] };
  const line = releaseLine(server.daemonVersion);
  const release = line === null ? null : yield* organizationReleaseChannel(organizationId).pipe(
    Effect.flatMap((channel) => channelRelease(channel, line)),
    Effect.timeout(OBSERVE_TIMEOUT),
    Effect.orElseSucceed(() => null),
  );
  const needs = `${feature.feature} ${feature.plural ? "need" : "needs"} a newer Ployz on ${server.name}.`;
  if (release === null) return needs;
  if (isBehind(server.daemonVersion, release)) return `${needs} ${release} is out.`;
  return isCurrent(server.daemonVersion, release) ? `${feature.feature} ${feature.plural ? "are" : "is"} not in Ployz ${server.daemonVersion} on ${server.name} yet.` : needs;
});

export const observeMembers = Effect.fn("VolumeRun.observe")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* observeVolumeMembers(ctx.organizationId, ctx.dockerVolume, ctx.kind);
});

/** Above every lease this Volume's runs took and every record its participants hold, so a Cloud database reset can't reuse one. */
export const takeLease = Effect.fn("VolumeRun.lease")(function* (
  ctx: RunContext,
  inngestRunId: string,
  participants: readonly AnsweredMember[],
  spec: ServiceSpec | null,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const { drizzle } = yield* Database;
  const held = Math.max(0, ...participants.map((member) => member.view.lease?.lease ?? 0));
  const [recorded] = yield* drizzle.select({ lease: max(volumeRun.lease) }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, ctx.volumeId), sql`${volumeRun.id} <> ${ctx.id}`));
  yield* drizzle.update(volumeRun)
    .set({ lease: Math.max(held, recorded?.lease ?? 0) + 1, serviceSpec: spec ?? undefined, updatedAt: new Date() })
    .where(and(eq(volumeRun.id, ctx.id), isNull(volumeRun.lease)));
  const row = yield* readRow(ctx.id);
  const lease = row?.lease;
  if (lease === null || lease === undefined) return yield* Effect.fail(new Error(`Volume run ${ctx.id} has no lease.`));
  return { lease };
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
  const database = yield* Database;
  const names = yield* findOrphanSlots(organizationId, observe);
  const started: string[] = [];
  for (const name of names) {
    const parsed = parseDockerVolumeName(name);
    if (parsed === null) continue;
    const row = yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [inserted] = yield* drizzle.insert(volumeRun).values({
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
      if (inserted !== undefined) yield* enqueueRun(inserted);
      return inserted;
    }));
    if (row === undefined) continue;
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
