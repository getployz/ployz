import "@tanstack/react-start/server-only";
import type {
  ContainerId,
  CopyObservation,
  CopyRefusal,
  DestructiveEffect,
  EnvironmentRef,
  ExpectedCopy,
  MachineId,
  MachineName,
  SnapshotGuid,
  SwitchError,
  VolumeCopy,
  VolumeSwitchReply,
  VolumeSwitchRequest,
  WantedCopy,
} from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, lt, max, sql } from "drizzle-orm";
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
import { alreadyOn, newestGuid, type Planned, planFromCopies, type Refusal, type RefusalCode } from "#/modules/volume-run/plan";
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
    epoch: row.epoch,
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
  no_writer: "conflict",
  two_writers: "conflict",
  volume_switching: "conflict",
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

/**
 * The copies a phase deletes with no other copy left: a Mirror Remove with no writer, or a Release that undoes a cut
 * and discards the copy its target pulled. `source` is the slot that keeps the cut's stopped Container.
 */
function phaseEffects(volume: ResolvedVolume, phase: PlannedPhase, source: MachineId | null): DestructiveEffect[] {
  switch (phase.kind) {
    case "delete_mirror":
      return phase.forget === null ? phase.destroy.map((member) => deletesCopy(volume, member)) : [];
    case "release": {
      const { from } = phase;
      if (from.kind !== "slots") return [];
      const kept = from.slots.find((member) => member.machine.id === source);
      if (kept === undefined) return [];
      const at = newestGuid(kept.view.copy);
      return from.slots.filter((member) => member !== kept && newestGuid(member.view.copy) === at).map((member) => deletesCopy(volume, member));
    }
    case "mirror":
    case "sync":
    case "move":
    case "landed":
    case "restore":
    case "demote":
    case "seed":
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
    const { code, message } = planned.refusal;
    return { ok: false, refusal: { code: refusalCode[code], message, details: null } } satisfies VolumeRunPlan;
  }
  const verb = VERBS[input.kind];
  const { phase } = planned;
  if (phase.kind === "landed") {
    const source = yield* keptOn(organizationId, volume.dockerVolume, phase.behind.map((member) => member.machine)).pipe(Effect.orElseSucceed(() => null));
    if (source === null) {
      const { code, message } = alreadyOn(volume.volumeName, phase.target.machine.name);
      return { ok: false, refusal: { code: refusalCode[code], message, details: null } } satisfies VolumeRunPlan;
    }
  }
  const source = phase.kind === "release" && phase.from.kind === "slots"
    ? yield* keptOn(organizationId, volume.dockerVolume, phase.from.slots.map((member) => member.machine)).pipe(Effect.orElseSucceed(() => null))
    : null;
  const effects = phaseEffects(volume, phase, source?.machine.id ?? null);
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

/** The reasons SeedVolume and the copy verbs may refuse with. */
export type SwitchReason = CopyRefusal | SwitchError["reason"];

const SWITCH_REASONS = [
  "mismatch", "no_transition", "busy", "no_capacity", "behind", "written", "no_source",
  "precondition", "volume_switching", "no_writer", "restore_point_gone",
] as const satisfies readonly SwitchReason[];

const SwitchErrorDetails = Schema.Struct({ details: Schema.Struct({ reason: Schema.Literals(SWITCH_REASONS) }) });
const decodeSwitchError = Schema.decodeUnknownOption(SwitchErrorDetails);

export function switchErrorOf(error: PloyzSdkError): SwitchReason | null {
  const decoded = decodeSwitchError("cause" in error ? error.cause : undefined);
  return Option.isSome(decoded) ? decoded.value.details.reason : null;
}

const CopyRefusedDetails = Schema.Struct({
  details: Schema.Struct({
    reason: Schema.Literals(["mismatch", "no_transition", "busy", "no_capacity", "behind", "written", "no_source"]),
    copy: Schema.NullOr(Schema.Unknown),
  }),
});
const decodeCopyRefused = Schema.decodeUnknownOption(CopyRefusedDetails);

export function switchFailureMessage(reason: SwitchReason, ctx: Pick<RunContext, "volumeName">, machine: MachineRef): string | undefined {
  const name = ctx.volumeName;
  switch (reason) {
    case "mismatch":
    case "precondition":
      return `${name}'s copy on ${machine.name} changed under this run`;
    case "no_transition":
      return `${name}'s copy on ${machine.name} can't get there from where it is`;
    case "behind":
      return `${name}'s copy on ${machine.name} lacks the snapshot this run needs`;
    case "written":
      return `${name} on ${machine.name} was written after its final snapshot`;
    case "no_source":
      return `${name}'s mirror on ${machine.name} has no Server to pull from`;
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

/** A failed attempt the step may retry; `reason` is the Machine's refusal, if it gave one. */
export class SwitchAttemptError extends Error {
  constructor(readonly reason: SwitchReason | null, message: string) {
    super(message);
  }
}

/** The verbs a run still sends through `sendSwitch`: SeedVolume and the reads. */
type SwitchRequest = Extract<VolumeSwitchRequest, { command: "seed_volume" | "inspect_volume_copy" }>;

/** A refusal with a message ends the run; Busy and transport failures fail only the attempt, so the step retries. */
export const sendSwitch = <R extends SwitchRequest>(ctx: RunContext, inngestRunId: string, machine: MachineRef, request: R) => {
  // SAFETY: a Machine answers each Volume Switch verb with that verb's reply; the session types the pair loosely.
  return Effect.gen(function* () {
    yield* requireOwner(ctx.id, inngestRunId);
    const session = yield* openSession(ctx.organizationId);
    return yield* session.volumeSwitch(machine.id, request).pipe(
      Effect.catch((error): Effect.Effect<never, Error, Database> => {
        const reason = switchErrorOf(error);
        const message = reason === null ? undefined : switchFailureMessage(reason, ctx, machine);
        if (message !== undefined) return failRun(ctx, inngestRunId, message);
        return Effect.fail(new SwitchAttemptError(reason, `${request.command} on ${machine.name}: ${sdkFailureMessage(error, [machine])}`));
      }),
    );
  }).pipe(Effect.scoped) as Effect.Effect<VolumeSwitchReply<R["command"]>, Error, Database | OrganizationRuntime>;
};

/** Plain JSON, since a step returns it: the copy the Machine holds now, after the step or after its refusal. */
export type CopyAnswer =
  | { readonly ok: true; readonly copy: VolumeCopy | null }
  | { readonly ok: false; readonly reason: CopyRefusal; readonly copy: VolumeCopy | null; readonly message: string };

/**
 * One SetVolumeCopy. A refusal is an answer, since the run decides what follows it; only a failure to ask fails the
 * attempt, so the step retries.
 */
export const setVolumeCopy = Effect.fn("VolumeRun.setVolumeCopy")(function* (
  ctx: RunContext,
  inngestRunId: string,
  machine: MachineRef,
  expect: ExpectedCopy,
  want: WantedCopy,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  return yield* session.volumeSwitch(machine.id, { command: "set_volume_copy", payload: { name: ctx.dockerVolume, expect, want } }).pipe(
    Effect.map((reply): CopyAnswer => ({ ok: true, copy: reply.copy })),
    Effect.catch((error) => {
      const refused = decodeCopyRefused("cause" in error ? error.cause : undefined);
      if (Option.isNone(refused)) {
        return Effect.fail(new SwitchAttemptError(null, `set_volume_copy on ${machine.name}: ${sdkFailureMessage(error, [machine])}`));
      }
      const { reason } = refused.value.details;
      // SAFETY: the Machine sends the copy it holds, as the SDK's VolumeCopy.
      const copy = refused.value.details.copy as VolumeCopy | null;
      const message = switchFailureMessage(reason, ctx, machine) ?? `${machine.name} is busy with ${ctx.volumeName}`;
      return Effect.succeed<CopyAnswer>({ ok: false, reason, copy, message });
    }),
  );
}, Effect.scoped);

export const inspectCopy = (ctx: RunContext, inngestRunId: string, machine: MachineRef) =>
  sendSwitch(ctx, inngestRunId, machine, { command: "inspect_volume_copy", payload: { name: ctx.dockerVolume } }).pipe(
    Effect.map((view) => view.copy),
  );

export type Holder = ServiceSpec & { readonly containerId: ContainerId };

/** Drain::DEFAULT: how long the cut lets requests in flight finish once the Container is out of ingress. */
export const DRAIN_MS = 3_000;

const serviceContainers = Effect.fn("VolumeRun.serviceContainers")(function* (organizationId: string, dockerVolume: string) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  return frame.containers.filter((container) =>
    container.kind === "service_container"
    && container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === dockerVolume));
}, Effect.scoped);

/** The one Service container on `machine` that mounts this Volume: the cut stops it, an undo starts it, and B's start copies its spec. */
export const findHolder = Effect.fn("VolumeRun.findHolder")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const holders = (yield* serviceContainers(ctx.organizationId, ctx.dockerVolume)).filter((container) => container.machine_id === machine.id);
  const [holder] = holders;
  if (holder === undefined || holders.length > 1) {
    const found = holders.length === 0 ? "no Service container" : `${holders.length} Service containers`;
    return { ok: false, refusal: { code: "invalid", message: `${ctx.volumeName} has ${found} on ${machine.name}; a move needs exactly one` } } as const;
  }
  const holderOf: Holder = {
    containerId: holder.container_id,
    namespace: holder.namespace,
    redactedSpec: holder.resolved_spec,
  };
  return { ok: true, holder: holderOf } as const;
});

/** Whether B already runs the Service, so a resent start can't be what makes it run. */
export const runningOn = Effect.fn("VolumeRun.runningOn")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  return (yield* serviceContainers(ctx.organizationId, ctx.dockerVolume))
    .some((container) => container.machine_id === machine.id && container.runtime.state === "running");
});

export type Kept = { readonly machine: MachineRef; readonly containerId: ContainerId };

/** With only slots left, the cut's source is the one of them that keeps a Service container for this Volume. */
export const keptOn = Effect.fn("VolumeRun.keptOn")(function* (organizationId: string, dockerVolume: string, machines: readonly MachineRef[]) {
  const kept = (yield* serviceContainers(organizationId, dockerVolume)).flatMap((container) => {
    const machine = machines.find((candidate) => candidate.id === container.machine_id);
    return machine === undefined ? [] : [{ machine, containerId: container.container_id } satisfies Kept];
  });
  return kept.length === 1 ? kept[0] ?? null : null;
});

export const findKept = Effect.fn("VolumeRun.findKept")(function* (ctx: RunContext, inngestRunId: string, machines: readonly MachineRef[]) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* keptOn(ctx.organizationId, ctx.dockerVolume, machines);
});

export const copyImage = Effect.fn("VolumeRun.copyImage")(function* (ctx: RunContext, inngestRunId: string, from: MachineRef, holder: Holder, to: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  yield* session.copyContainerImage(from.id, holder.containerId, to.id).pipe(
    Effect.mapError((error) => new Error(`copying ${holder.redactedSpec.name}'s image to ${to.name}: ${sdkFailureMessage(error, [from, to])}`)),
  );
}, Effect.scoped);

/** Keyed on the Volume and G, so a resend after a crash finds the Container the first send made. */
export const creationKey = (ctx: Pick<RunContext, "volumeId">, at: SnapshotGuid) => `${ctx.volumeId}-${at}`;

/**
 * B becomes the writer at G and creates the Service from A's stopped Container. The spec is read and sent inside one
 * step, so it never lands in a step result.
 */
export const startOnTarget = Effect.fn("VolumeRun.startOnTarget")(function* (
  ctx: RunContext,
  inngestRunId: string,
  move: { readonly from: MachineRef; readonly holder: Holder; readonly to: MachineRef; readonly expect: ExpectedCopy; readonly at: SnapshotGuid; readonly epoch: number },
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const { from, holder, to } = move;
  const session = yield* openSession(ctx.organizationId);
  const { container } = yield* session.inspectContainer(from.id, holder.containerId).pipe(
    Effect.mapError((error) => new Error(`reading ${holder.redactedSpec.name}'s spec on ${from.name}: ${sdkFailureMessage(error, [from])}`)),
  );
  const spec = container.resolved_spec;
  return yield* setVolumeCopy(ctx, inngestRunId, to, move.expect, {
    kind: "writer",
    at: move.at,
    epoch: move.epoch,
    start: {
      kind: "create",
      namespace: holder.namespace,
      resolved_spec: { ...spec, container: { ...spec.container, pull_policy: "never" } },
      creation_key: creationKey(ctx, move.at),
    },
  });
}, Effect.scoped);

export const removeContainer = Effect.fn("VolumeRun.removeContainer")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef, containerId: ContainerId) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  yield* session.removeContainer(machine.id, containerId).pipe(
    Effect.mapError((error) => new Error(`removing ${ctx.volumeName}'s old Container on ${machine.name}: ${sdkFailureMessage(error, [machine])}`)),
  );
}, Effect.scoped);

/** Each managed Server's copy of `dockerVolume`; a Server that doesn't answer in time is a member that didn't answer. */
export const observeVolumeMembers = Effect.fn("VolumeRun.observeMembers")(function* (organizationId: string, dockerVolume: string, kind?: VolumeRunKind) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  // A Docker-only Machine holds no copy and may not serve the verb, so it must not block the run.
  const managed = frame.machines.filter((observed) => observed.storage?.state !== "stateless");
  return yield* Effect.forEach(managed, ({ machine }) =>
    session.volumeSwitch(machine.id, { command: "inspect_volume_copy", payload: { name: dockerVolume } }).pipe(
      Effect.map((view): Member => ({
        machine: { id: machine.id, name: machine.name },
        address: managementAddress(machine.public_key),
        answered: true,
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

/**
 * The epoch the run gives a new writer: above every epoch the Machines show and every epoch this Volume's runs took,
 * so neither a Cloud database reset nor a Machine reset can reuse one. Taken once; a replay reads it back.
 */
export const takeEpoch = Effect.fn("VolumeRun.epoch")(function* (ctx: RunContext, inngestRunId: string, observed: number) {
  yield* requireOwner(ctx.id, inngestRunId);
  const { drizzle } = yield* Database;
  const [recorded] = yield* drizzle.select({ epoch: max(volumeRun.epoch) }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, ctx.volumeId), sql`${volumeRun.id} <> ${ctx.id}`));
  yield* drizzle.update(volumeRun)
    .set({ epoch: Math.max(observed, recorded?.epoch ?? 0) + 1, updatedAt: new Date() })
    .where(and(eq(volumeRun.id, ctx.id), isNull(volumeRun.epoch)));
  const row = yield* readRow(ctx.id);
  const epoch = row?.epoch;
  if (epoch === null || epoch === undefined) return yield* Effect.fail(new Error(`Volume run ${ctx.id} has no epoch.`));
  return { epoch };
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
