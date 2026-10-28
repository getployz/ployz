import "@tanstack/react-start/server-only";
import type {
  ContainerId,
  CopyObservation,
  CopyRefusal,
  DestructiveEffect,
  DockerVolume,
  EnvironmentRef,
  ExpectedCopy,
  MachineId,
  MachineName,
  SnapshotGuid,
  SwitchError,
  VolumeCopy,
  VolumeKind,
  VolumeSwitchRequest,
  WantedCopy,
} from "@ployz/sdk";
import { and, desc, eq, inArray, isNotNull, isNull, lt, max, ne, notInArray, or, type SQL, sql } from "drizzle-orm";
import { Data, Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import type { OperationVerb } from "#/modules/approvals/approvals";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { JsonObject, JsonValue } from "#/db/tables";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { createVolumeRunRequestedEvent, createVolumeRunTakenEvent } from "#/modules/inngest/events";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { organizationMachine } from "#/modules/machines/tables";
import {
  ClusterOutOfTime,
  clusterRead,
  OrganizationRuntime,
  PLAN_CLUSTER_BUDGET_MS,
  requireClusterTime,
  RUNTIME_FRAME_TIMEOUT_MS,
  withClusterBudget,
} from "#/modules/runtime/organization-runtime.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { movedRouting, type Requirement, requirementFor, requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { observeVolumeCopies, type PloyzSdkError, RpcError, sdkFailureMessage, setVolumeCopyRefusal } from "#/modules/runtime/ployz.server";
import {
  alreadyOn, newestGuid, type PlanInput, type Planned, planForwardRelease, planFromCopies, type Refusal, type RefusalCode, type ReleaseFrom, restoreSource,
  roleOf, writerRemovedMessage,
} from "#/modules/volume-run/plan";
import { type DeploymentsSeen, deployingMessage, gb, resizeRequirements, sizeDirection } from "#/modules/volume-run/resize";
import { isBehind, isCurrent, releaseLine } from "#/modules/server-upgrade/server-upgrade";
import { channelRelease, organizationReleaseChannel } from "#/modules/server-upgrade/server-upgrade.server";
import { volumeRun } from "#/modules/volume-run/tables";
import {
  ACTIVE_VOLUME_RUN_STATES,
  type AnsweredMember,
  copyName,
  dockerVolumeName,
  isDestination,
  managementAddress,
  type Member,
  needsFloor,
  parseDockerVolumeName,
  RUN_FEATURES,
  type ServiceSpec,
  VOLUME_RUN_MESSAGE_LIMIT,
  VOLUME_RUN_RUNNING_CHECK_MS,
  VOLUME_RUN_UNCLAIMED_LIMIT_MS,
  type VolumeLaneInput,
  type VolumeRunArgs,
  type VolumeRunInput,
  type VolumeRunKind,
  type VolumeRunPhase,
  type VolumeRunState,
  type VolumeRunView,
} from "#/modules/volume-run/volume-run";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

type VolumeRunRow = typeof volumeRun.$inferSelect;
type PlannedPhase = Extract<Planned, { ok: true }>["phase"];

type RunBase = {
  readonly id: string;
  readonly organizationId: string;
  readonly environmentId: string;
  readonly volumeId: string;
  readonly volumeName: string;
  readonly dockerVolume: string;
  readonly orphan: boolean;
  readonly refquotaBytes: number;
  readonly recordedBytes: number;
  readonly gen: number;
  readonly phase: VolumeRunPhase;
  readonly current: MachineId | null;
};

/** Plain JSON: Inngest memoizes step output as JSON. */
export type RunContext = RunBase & VolumeLaneInput;

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

export function laneInputOf(lane: Pick<VolumeRunRow, "kind" | "args">): VolumeLaneInput {
  // SAFETY: the row's kind and args were written together from one VolumeLaneInput.
  return { kind: lane.kind, args: lane.args } as VolumeLaneInput;
}

export const contextOf = (row: VolumeRunRow): RunContext => ({
  ...laneInputOf(row),
  id: row.id,
  organizationId: row.organizationId,
  environmentId: row.environmentId,
  volumeId: row.volumeId,
  volumeName: row.volumeName,
  dockerVolume: row.dockerVolume,
  orphan: row.orphan,
  refquotaBytes: row.refquotaBytes,
  recordedBytes: row.recordedBytes,
  gen: row.gen,
  phase: row.phase,
  current: row.current,
});

export const ended = (state: Exclude<VolumeRunState, "requested" | "running">, message: string | null) =>
  ({ state, message: message === null ? null : clip(message), finishedAt: new Date(), updatedAt: new Date() }) as const;

const KIND_WORDS = {
  mirror: "mirror",
  sync: "mirror refresh",
  delete_mirror: "mirror removal",
  move: "move",
  release: "release",
  restore: "restore",
  seed: "restore",
  resize: "resize",
  deploy: "deploy",
} satisfies Record<VolumeRunKind, string>;

// A Machine may already be destroying the copy, and no floor retracts that, so a release cannot promise to stop it.
export const volumeBusyMessage = (lane: Pick<VolumeRunRow, "id" | "kind" | "volumeName">) => {
  const follow = `ployz volume runs ${lane.volumeName} ${lane.id} follows it`;
  if (lane.kind === "delete_mirror") return `${lane.volumeName} is busy with a mirror removal, which finishes on its own; ${follow}`;
  if (lane.kind === "release") return `${lane.volumeName} is busy with a release; ${follow}`;
  return `${lane.volumeName} is busy with a ${KIND_WORDS[lane.kind]}; ${follow}, and ployz volume release ${lane.volumeName} stops it`;
};

export const finishesThere = (volumeName: string, server: string) => `${volumeName} is starting on ${server}; it finishes there`;

/** A lane past its point of no return: a Move that committed, or a restore, which commits from its first step. */
export const pastReturn = (lane: Pick<VolumeRunRow, "phase" | "kind">) => lane.phase === "committed" || lane.kind === "restore";
type LaneEnd = "cancelled" | "yielded";
// A seed's copy into the new Volume runs on without its row, and a Machine destroying a mirror's copy runs on without
// its removal's, so a release could only stop watching them. A Deploy's Services run on its lane, or beside a lane it
// holds, so only a Deploy takes either.
const keepsLane = (lane: VolumeRunRow, end: LaneEnd, kind: VolumeRunKind) =>
  pastReturn(lane) || (deploying(lane) && kind !== "deploy") || ((lane.kind === "seed" || lane.kind === "delete_mirror") && end === "cancelled");

export const laneTarget = Effect.fn("VolumeRun.laneTarget")(function* (lane: Pick<VolumeRunRow, "volumeId" | "kind" | "args" | "current">) {
  const input = laneInputOf(lane);
  if (input.kind === "restore") return input.args.from;
  if (input.kind === "move") return input.args.to;
  const { current } = lane;
  if (current === null) return null;
  const { drizzle } = yield* Database;
  const [move] = yield* drizzle.select({ kind: volumeRun.kind, args: volumeRun.args }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, lane.volumeId), eq(volumeRun.kind, "move"), eq(volumeRun.current, current)))
    .orderBy(desc(volumeRun.createdAt))
    .limit(1);
  if (move === undefined) return null;
  const moved = laneInputOf(move);
  return moved.kind === "move" ? moved.args.to : null;
});

const activeLane = Effect.fn("VolumeRun.activeLane")(function* (volumeId: string, lock = false) {
  const { drizzle } = yield* Database;
  const query = drizzle.select().from(volumeRun).where(and(eq(volumeRun.volumeId, volumeId), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES)));
  const [lane] = lock ? yield* query.for("update") : yield* query;
  return lane;
});

const deploying = (lane: Pick<VolumeRunRow, "kind" | "heldBy">) => lane.kind === "deploy" || lane.heldBy !== null;

/**
 * The Volume's name now, read by the ids its Docker volume name carries, so a rename after the request resolved still
 * shows: a run keeps the name it started under. A Volume no longer configured keeps that name, and so does one whose
 * Project was renamed or Environment removed between the two reads.
 */
const currentVolumeName = Effect.fn("VolumeRun.currentVolumeName")(function* (run: Pick<VolumeRunRow, "organizationId" | "dockerVolume" | "volumeName">) {
  const parsed = parseDockerVolumeName(run.dockerVolume);
  if (parsed === null) return run.volumeName;
  const { namespaces } = yield* readStore(run.organizationId, { query: "namespaces" });
  const owned = namespaces.find((entry) => entry.namespace === parsed.namespace);
  if (owned === undefined) return run.volumeName;
  const named = yield* readStore(run.organizationId, { query: "volumes", environment: { project: owned.project, environment: owned.environment } }).pipe(
    Effect.map(({ volumes }) => volumes.find((volume) => volume.id === parsed.volumeId)?.name),
    Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(undefined)),
  );
  return named ?? run.volumeName;
});

/** Names the Volume as it is called at the refusal, so the hints run as printed. */
const laneRefusal = Effect.fn("VolumeRun.laneRefusal")(function* (lane: VolumeRunRow, kind: VolumeRunKind) {
  const run = volumeRunView(lane);
  const volumeName = yield* currentVolumeName(lane);
  if (deploying(lane)) return { code: "conflict", message: `${volumeName} is deploying; the Deploy ends its run`, details: { run } } satisfies StoreRefusal;
  const refuse = (message: string, details: JsonValue = { run }) => ({ code: "conflict", message, details }) satisfies StoreRefusal;
  const input = laneInputOf(lane);
  if (input.kind === "seed") return refuse(`${volumeName} is being copied into ${input.args.into.name}; it finishes on its own`);
  if (!pastReturn(lane)) return refuse(volumeBusyMessage({ id: lane.id, kind: lane.kind, volumeName }));
  const target = yield* laneTarget(lane);
  const finishes = finishesThere(volumeName, target ?? "its new Server");
  // A restore's target is the copy it restores from, not a move's destination.
  if (kind === "release" && target !== null && lane.kind !== "restore") return refuse(finishes, { run, move_to: target });
  return refuse(finishes);
});

/**
 * The rows that change whether a committed Move is still owed its finish: a commitment, a move or release that ended
 * done, and a restore that took an epoch. The newest of them by gen decides.
 */
const MEANINGFUL = or(
  and(inArray(volumeRun.kind, ["move", "release"]), or(and(eq(volumeRun.phase, "committed"), isNotNull(volumeRun.current)), eq(volumeRun.state, "done"))),
  and(eq(volumeRun.kind, "restore"), or(isNotNull(volumeRun.epoch), eq(volumeRun.state, "done"))),
);

type Owed = VolumeRunRow & { readonly current: MachineId };

/** A commitment no run holds any more: its Volume finishes on `current` once a release or a Deploy starts it there. */
const owed = (row: VolumeRunRow | undefined): Owed | null =>
  row !== undefined && row.phase === "committed" && row.current !== null && row.state !== "done" && !ACTIVE_VOLUME_RUN_STATES.some((active) => active === row.state)
    ? { ...row, current: row.current }
    : null;

const newestMeaningful = Effect.fn("VolumeRun.newestMeaningful")(function* (volumeId: string, organizationId: string | undefined, lock: boolean) {
  const { drizzle } = yield* Database;
  const inOrganization = organizationId === undefined ? undefined : eq(volumeRun.organizationId, organizationId);
  const newestFirst = drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.volumeId, volumeId), inOrganization, MEANINGFUL)).orderBy(desc(volumeRun.gen)).limit(1);
  const [newest] = yield* (lock ? newestFirst.for("update") : newestFirst);
  return { newest, owed: owed(newest) };
});

export const commitment = (volumeId: string, organizationId?: string) => newestMeaningful(volumeId, organizationId, false);

/** The commitment, its row held until the transaction ends. */
const commitmentHeldAgainstRemoval = (volumeId: string) => newestMeaningful(volumeId, undefined, true);

/** Each of the Environment's Volumes whose committed Move is still owed its finish. */
export const owedCommitments = Effect.fn("VolumeRun.owedCommitments")(function* (organizationId: string, environmentId: string) {
  const { drizzle } = yield* Database;
  const newest = yield* drizzle.selectDistinctOn([volumeRun.volumeId]).from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.environmentId, environmentId), MEANINGFUL))
    .orderBy(volumeRun.volumeId, desc(volumeRun.gen));
  return newest.flatMap((row) => owed(row) ?? []);
});

// A sync's cut assumes a writer, and an owed Volume has none until the finish starts one on B. A resize asks every
// copy, and the one the Move finishes on is not yet the writer.
const OWED_KINDS = {
  move: true, mirror: true, seed: true, sync: true, resize: true, delete_mirror: false, release: false, restore: false, deploy: false,
} as const satisfies Record<VolumeRunKind, boolean>;

/**
 * A run that would undo or copy around a Move still owed its finish, or destroy the copy it finishes on. That copy is
 * known by its Machine's id, since the Server may be renamed, so a removal whose slot resolved to no id is refused.
 */
const refusedWhileOwed = (input: VolumeLaneInput, pending: Owed) => {
  if (OWED_KINDS[input.kind]) return true;
  if (input.kind !== "delete_mirror") return false;
  return input.args.machine === undefined || input.args.machine === pending.current;
};

const owedRefusal = Effect.fn("VolumeRun.owedRefusal")(function* (pending: Owed, target: string | null) {
  const volumeName = yield* currentVolumeName(pending);
  return {
    code: "conflict",
    message: `${volumeName} finishes on ${target ?? "its new Server"}; ployz volume release ${volumeName} finishes it now`,
    details: { run: volumeRunView(pending) },
  } satisfies StoreRefusal;
});

const namedOwedRefusal = Effect.fn("VolumeRun.namedOwedRefusal")(function* (pending: Owed) {
  const { name } = yield* machineNamed(pending.organizationId, pending.current);
  return yield* owedRefusal(pending, name ?? (yield* laneTarget(pending)));
});

type DeleteMirrorArgs = VolumeRunArgs["delete_mirror"];

const withSlotMachine = (args: DeleteMirrorArgs, machines: readonly MachineRef[]): DeleteMirrorArgs => {
  const machine = machines.find((entry) => entry.name === args.slot)?.id;
  return machine === undefined ? args : { ...args, machine };
};

export const releaseChangedMessage = (volumeName: string) => `${volumeName} changed since this release was planned; release it again`;

const releaseChanged = (volumeName: string): StoreRefusal => ({ code: "conflict", message: releaseChangedMessage(volumeName), details: null });

const followSettled = (volumeName: string): StoreRefusal => ({
  code: "conflict",
  message: `${volumeName} no longer finishes on the Server this Deploy followed`,
  details: { reason: "follow_settled" },
});

/** Whether a Deploy's release was refused because the commitment it followed had settled. */
export const refusedAsSettled = Schema.is(Schema.Struct({ details: Schema.Struct({ reason: Schema.Literal("follow_settled") }) }));

const enqueueRun = (row: VolumeRunRow, took?: VolumeRunKind) =>
  enqueueEvent(createVolumeRunRequestedEvent({
    organizationId: row.organizationId,
    environment: row.environmentId,
    volumeId: row.volumeId,
    runId: row.id,
    took,
  }));

export const readRow = Effect.fn("VolumeRun.readRow")(function* (id: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(volumeRun).where(eq(volumeRun.id, id));
  return row;
});

type ResolvedVolume = Omit<Effect.Success<ReturnType<typeof resolveVolume>>, "environment">;

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
    environment: { project: summary.project, name: summary.name },
    volumeName: volume.name,
    dockerVolume: dockerVolumeName(owned.namespace, volume.id),
    refquotaBytes: volume.storage.kind === "provisioned" ? volume.storage.maximumBytes : 0,
    recordedBytes: volume.recorded_storage ?? 0,
  };
});

/**
 * The storage a Volume holds: the size the Store recorded for a deployed Managed Volume, which a staged older revision
 * leaves as it is, else what Working State asks.
 */
export const heldStorage = (volume: { readonly storage: VolumeKind; readonly recorded_storage?: number }): VolumeKind =>
  volume.recorded_storage === undefined ? volume.storage : { kind: "provisioned", maximumBytes: volume.recorded_storage };

/** A seed copies the source's dataset as is, so the restored Volume must still hold the source's storage. */
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
  if (JSON.stringify(heldStorage(source)) === JSON.stringify(heldStorage(into))) return null;
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
  already_on: "conflict",
  held_below: "conflict",
  unobserved: "unavailable",
  no_writer: "conflict",
  two_writers: "conflict",
  volume_switching: "conflict",
  second_mirror: "conflict",
  no_mirror: "conflict",
  no_copy: "conflict",
  another_copy: "conflict",
  role_changed: "conflict",
  deploying: "conflict",
  server_lacks: "unsupported",
} satisfies Record<RefusalCode, string>;

/** A plan's refusal as the Store hears it; a call whose outcome already holds is settled, so the Store offers no retry. */
export const storeRefusal = ({ code, message, next }: Refusal): StoreRefusal => {
  if (code === "already_on") return { code: refusalCode[code], message, details: { settled: true } };
  return { code: refusalCode[code], message, details: next === undefined ? null : { next } };
};

const deletesCopy = (volume: ResolvedVolume, member: AnsweredMember): DestructiveEffect => ({
  kind: "deletes_volume",
  name: copyName(volume.volumeName, member.machine.name),
  node: `${member.machine.id}/${volume.dockerVolume}`,
  path: `volumes/${member.machine.id}/${volume.dockerVolume}`,
});

type PlannedRelease = { readonly sources: readonly AnsweredMember[]; readonly forgets: readonly AnsweredMember[] };

/**
 * The slots a Release may resume from and forget. A Release from slots resumes where the stopped Container is and
 * forgets the slots at its snapshot. A source the plan could not read may be any slot, so every slot may go; a read
 * that found no single stopped Container resumes nowhere.
 */
function releasePlan(from: ReleaseFrom | null, source: Option.Option<MachineId | null>): PlannedRelease {
  if (from?.kind !== "slots") return { sources: [], forgets: [] };
  if (Option.isNone(source)) return { sources: from.slots, forgets: from.slots };
  const kept = from.slots.find((member) => member.machine.id === source.value);
  if (kept === undefined) return { sources: [], forgets: [] };
  const at = newestGuid(kept.view.copy);
  return { sources: [kept], forgets: from.slots.filter((member) => member !== kept && newestGuid(member.view.copy) === at) };
}

/**
 * The copies a phase deletes with no other copy left: a Mirror Remove with no writer, or a Release that undoes a cut
 * and discards the copies its plan `forgets`.
 */
function phaseEffects(volume: ResolvedVolume, phase: PlannedPhase, forgets: readonly AnsweredMember[]): DestructiveEffect[] {
  switch (phase.kind) {
    case "delete_mirror":
      return phase.forget === null ? phase.destroy.map((member) => deletesCopy(volume, member)) : [];
    case "release":
      return forgets.map((member) => deletesCopy(volume, member));
    case "mirror":
    case "sync":
    case "move":
    case "landed":
    case "restore":
    case "demote":
    case "seed":
    case "resize":
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
  resize: null,
  deploy: null,
} satisfies Record<VolumeRunKind, OperationVerb | null>;

/**
 * The verb a run asks under in a Protected Environment, for one by someone who can't protect it that interrupts, takes
 * off or resizes what runs there: a move stops its Service for the last copy, a restore drops writes, and a resize changes
 * live storage, where a grow can't shrink back. It asks whatever step the run plans first, since the worker plans again:
 * a run admitted to seal an older second primary may move or resize once that copy is gone. A mirror, a sync and a seed
 * leave what runs alone.
 */
const PROTECTED_VERBS = {
  ...VERBS,
  move: "move",
  restore: "restore",
  resize: "resize",
} satisfies Record<VolumeRunKind, OperationVerb | null>;

/**
 * What a guarded run was asked to do, as its approval binds it: the Server it moves to or restores from, or the sizes it
 * resizes from and to. It comes from the request, not the step admission plans first, so an approval holds whichever
 * step the worker reaches and covers no other Server or size.
 */
function requestPreview(volume: ResolvedVolume, input: VolumeRunInput, members: readonly Member[]): JsonObject {
  // A Server by its name and the Machine that name resolves to now, or the one the request named by id.
  const server = (name: string, machine: string | undefined) =>
    ({ name, machine: machine ?? members.find((member) => member.machine.name === name)?.machine.id ?? null });
  switch (input.kind) {
    case "move":
      return { to: server(input.args.to, input.args.machine) };
    case "restore":
      return { from: server(input.args.from, input.args.machine) };
    case "resize":
      return { recorded_bytes: volume.recordedBytes, maximum_bytes: input.args.maximum_bytes };
    case "mirror":
    case "sync":
    case "delete_mirror":
    case "release":
    case "seed":
      return {};
  }
}

/** What a guarded run acts on as its approval names it: "pg-data to fsn-2", "pg-data from 10 GB to 20 GB". */
function requestName(volume: ResolvedVolume, input: VolumeRunInput): string {
  switch (input.kind) {
    case "move":
      return `${volume.volumeName} to ${input.args.to}`;
    case "restore":
      return `${volume.volumeName} from ${input.args.from}`;
    case "resize":
      return volume.recordedBytes === 0
        ? `${volume.volumeName} to ${gb(input.args.maximum_bytes)}`
        : `${volume.volumeName} from ${gb(volume.recordedBytes)} to ${gb(input.args.maximum_bytes)}`;
    case "mirror":
    case "sync":
    case "delete_mirror":
    case "release":
    case "seed":
      return volume.volumeName;
  }
}

type PlanRequest = { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput;

/**
 * `input` is the request to insert: a Release carries the plan its approval showed. `guard` is the ask a Protected
 * Environment makes of someone who can't protect it, when the run interrupts what runs there.
 */
type ClusterPlan =
  | { readonly ok: true; readonly volume: ResolvedVolume; readonly asked: OperationAsked | null; readonly guard: OperationAsked | null; readonly input: PlanRequest }
  | { readonly ok: false; readonly refusal: StoreRefusal };

/** A plan as `planVolumeRun` answers it, naming the Environment the Volume is in. */
export type VolumeRunPlan =
  | (Extract<ClusterPlan, { ok: true }> & { readonly environment: { readonly project: string; readonly name: string } })
  | Extract<ClusterPlan, { ok: false }>;

const unanswered: StoreRefusal = { code: "unavailable", message: "Your servers aren't answering. Try again once they are.", details: null };

/**
 * A requested run planned against the copies the Servers hold now, so a refusal reaches the caller before any row
 * exists. The run plans again when it starts, since the copies may move in between.
 */
export const planVolumeRun = Effect.fn("VolumeRun.plan")(function* (
  organizationId: string,
  input: PlanRequest,
) {
  const { environment, ...volume } = yield* resolveVolume(organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal } satisfies VolumeRunPlan;
  }
  const lane = yield* activeLane(input.volumeId);
  if (lane !== undefined && (input.kind !== "release" || keepsLane(lane, "cancelled", "release"))) {
    return { ok: false, refusal: yield* laneRefusal(lane, input.kind) } satisfies VolumeRunPlan;
  }
  const { owed: pending } = yield* commitment(input.volumeId);
  if (pending !== null && input.kind !== "delete_mirror" && refusedWhileOwed(input, pending)) {
    return { ok: false, refusal: yield* namedOwedRefusal(pending) } satisfies VolumeRunPlan;
  }
  if (input.kind === "resize" && sizeDirection(volume.recordedBytes, input.args.maximum_bytes) === "shrink") {
    const refusal = yield* shrinkWhileDeploying(organizationId, input.environment, volume.volumeName, input.args.maximum_bytes);
    if (refusal !== null) return { ok: false, refusal } satisfies VolumeRunPlan;
  }
  const planned = yield* planOnCluster(organizationId, volume, input, pending);
  return planned.ok ? { ...planned, environment } : planned;
});

/**
 * A shrink asked while a Deployment of the Environment is in flight, refused before any row in the words the Store
 * refuses its record in. The Store's refusal is the authority; this one only spares a run that would fail at its record.
 */
const shrinkWhileDeploying = Effect.fn("VolumeRun.shrinkWhileDeploying")(function* (
  organizationId: string,
  environment: EnvironmentRef,
  volumeName: string,
  to: number,
) {
  const { in_flight: inFlight } = yield* readStore(organizationId, { query: "deployments", environment, limit: 1, cursor: null });
  const [running] = inFlight;
  if (running === undefined) return null;
  return { code: "conflict", message: deployingMessage(volumeName, running.number, to), details: { reason: "deploying", deployment: { id: running.id, number: running.number } } } satisfies StoreRefusal;
});

/**
 * The Environment's Deployments as a resize reads them before it asks the Servers: a record of a Volume no Server
 * holds names the newest it saw, and refuses once a newer one exists.
 */
const deploymentsSeen = Effect.fn("VolumeRun.deploymentsSeen")(function* (organizationId: string, environment: EnvironmentRef) {
  const { latest, in_flight: inFlight } = yield* readStore(organizationId, { query: "deployments", environment, limit: 1, cursor: null });
  const [running] = inFlight;
  return { latest: latest?.number ?? null, inFlight: running === undefined ? null : { number: running.number } } satisfies DeploymentsSeen;
});

/**
 * A resize run's Deployments, read before it observes the Servers; null when the Environment its Docker volume names
 * is gone, which a Volume no Server holds plans as unread.
 */
export const runDeploymentsSeen = Effect.fn("VolumeRun.runDeploymentsSeen")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  const parsed = parseDockerVolumeName(ctx.dockerVolume);
  if (parsed === null) return null;
  const { namespaces } = yield* readStore(ctx.organizationId, { query: "namespaces" });
  const owned = namespaces.find((entry) => entry.namespace === parsed.namespace);
  if (owned === undefined) return null;
  return yield* deploymentsSeen(ctx.organizationId, { project: owned.project, environment: owned.environment });
});

/** A read's failure as none, except the plan running out of Cluster time, which refuses the whole plan. */
const answered = <A, E, R>(read: Effect.Effect<A, E, R>) => read.pipe(
  Effect.map(Option.some),
  Effect.catch((error) => error instanceof ClusterOutOfTime ? Effect.fail(error) : Effect.succeedNone),
);

/**
 * The rest of a plan, read off the copies the Cluster reports. Its Cluster reads share one budget, so a plan that opens
 * two sessions still answers inside the CLI's request, and one that runs out of time says the Servers aren't answering.
 */
export const planOnCluster = Effect.fn("VolumeRun.planOnCluster")(function* (
  organizationId: string,
  volume: ResolvedVolume,
  input: PlanRequest,
  pending: Owed | null,
) {
  // Read before the Servers are asked: a Deploy admitted after this may create a copy the observation missed.
  const deployments = input.kind === "resize" ? Option.getOrUndefined(yield* answered(deploymentsSeen(organizationId, input.environment))) : undefined;
  const members = yield* answered(observeVolumeMembers(organizationId, volume.dockerVolume, input.kind === "resize" ? { ...input, recordedBytes: volume.recordedBytes } : input));
  if (Option.isNone(members)) return { ok: false, refusal: unanswered } satisfies ClusterPlan;
  if (pending !== null && input.kind === "release") return yield* planForward(volume, input, pending, members.value);
  const resolved = input.kind === "delete_mirror"
    ? { ...input, args: withSlotMachine(input.args, members.value.map((member) => member.machine)) }
    : input;
  if (pending !== null && refusedWhileOwed(resolved, pending)) return { ok: false, refusal: yield* namedOwedRefusal(pending) } satisfies ClusterPlan;
  if (pending !== null && input.kind === "delete_mirror" && members.value.some((member) => member.machine.id !== pending.current && roleOf(member) === "cutting")) {
    const forward = planForwardRelease(volume.volumeName, yield* owedTarget(pending, members.value), members.value);
    if (forward.kind === "blocked") return { ok: false, refusal: { code: "conflict", message: forward.message, details: null } } satisfies ClusterPlan;
  }
  const planInput: PlanInput = input.kind === "resize"
    ? { ...input, volumeName: volume.volumeName, orphan: false, recordedBytes: volume.recordedBytes, refquotaBytes: volume.refquotaBytes, ...(deployments && { deployments }) }
    : { ...input, volumeName: volume.volumeName, orphan: false };
  const planned = planFromCopies(planInput, members.value);
  if (!planned.ok) {
    return { ok: false, refusal: storeRefusal(planned.refusal) } satisfies ClusterPlan;
  }
  const verb = VERBS[input.kind];
  const { phase } = planned;
  if (phase.kind === "landed") {
    const source = yield* answered(keptOn(organizationId, volume.dockerVolume, phase.behind.map((member) => member.machine)));
    if (Option.isNone(source)) return { ok: false, refusal: unanswered } satisfies ClusterPlan;
    if (source.value === null) {
      return { ok: false, refusal: storeRefusal(alreadyOn(volume.volumeName, phase.target.machine.name)) } satisfies ClusterPlan;
    }
  }
  const from = phase.kind === "release" ? phase.from : null;
  const source = from?.kind === "slots"
    ? yield* answered(keptOn(organizationId, volume.dockerVolume, from.slots.map((member) => member.machine)).pipe(
      Effect.map((kept) => kept?.machine.id ?? null),
    ))
    : Option.some(null);
  const release = releasePlan(from, source);
  const effects = phaseEffects(volume, phase, release.forgets);
  const askedAs = (verb: OperationVerb): OperationAsked => ({
    subject: `volume:${input.volumeId}`,
    verb,
    name: requestName(volume, input),
    preview: { volume: input.volumeId, copies: effects.map((effect) => effect.node), ...requestPreview(volume, input, members.value) },
    effects,
  });
  const asked = verb === null ? null : askedAs(verb);
  const guarding = PROTECTED_VERBS[input.kind];
  const guard = asked ?? (guarding === null ? null : askedAs(guarding));
  const ids = (members: readonly AnsweredMember[]) => members.map((member) => member.machine.id);
  const pinned = input.kind === "release" ? { ...input, args: { plan: { sources: ids(release.sources), forgets: ids(release.forgets) } } } : resolved;
  return { ok: true, volume, asked, guard, input: pinned } satisfies ClusterPlan;
}, Effect.catchTag("ClusterOutOfTime", () => Effect.succeed({ ok: false, refusal: unanswered } satisfies ClusterPlan)), withClusterBudget(PLAN_CLUSTER_BUDGET_MS));

const owedTarget = Effect.fn("VolumeRun.owedTarget")(function* (pending: Owed, members: readonly Member[]) {
  return members.find((member) => member.machine.id === pending.current)?.machine
    ?? { id: pending.current, name: (yield* laneTarget(pending)) ?? pending.current };
});

/**
 * A release that finishes a committed Move on its new Server, pinned to the commitment and the Servers it previewed.
 */
const planForward = Effect.fn("VolumeRun.planForward")(function* (volume: ResolvedVolume, input: PlanRequest, pending: Owed, members: readonly Member[]) {
  const to = yield* owedTarget(pending, members);
  const forward = planForwardRelease(volume.volumeName, to, members);
  if (forward.kind === "blocked") return { ok: false, refusal: { code: "conflict", message: forward.message, details: null } } satisfies ClusterPlan;
  const asked: OperationAsked = {
    subject: `volume:${input.volumeId}`,
    verb: "release",
    name: volume.volumeName,
    preview: { volume: input.volumeId, direction: "forward", to: { machine: to.id, name: to.name }, remove_containers_on: forward.removeOn.map((machine) => machine.name) },
    effects: [],
  };
  const plan = { sources: [], forgets: [], forward: { after: pending.id, to: pending.current, removeOn: forward.removeOn.map((machine) => machine.id) } };
  return { ok: true, volume, asked, guard: asked, input: { ...input, kind: "release", args: { plan } } } satisfies ClusterPlan;
});

export type RequestOptions = {
  readonly id?: string;
  /** `yielded` takes over a warm lane as a Deploy or a staged placement does; a release always cancels. */
  readonly end?: "yielded";
  /** The kind of a fenced run this one replaces: it fences before it plans even when it takes over no lane. */
  readonly fences?: VolumeRunKind;
};

export const requestVolumeRun = Effect.fn("VolumeRun.request")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
  options: RequestOptions = {},
) {
  const volume = yield* resolveVolume(caller.organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(caller.organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal, busy: null } as const;
  }
  const resolved = input.kind === "delete_mirror" && input.args.slot !== null && input.args.machine === undefined
    ? { ...input, args: withSlotMachine(input.args, yield* namedMachines(caller.organizationId)) }
    : input;
  return yield* insertRun(caller, resolved, volume, options.id ?? crypto.randomUUID(), options.end ?? endOf(input.kind), options.fences);
});

export type LaneTaker = {
  readonly input: { readonly volumeId: string } & VolumeLaneInput;
  readonly volume: ResolvedVolume;
  readonly id: string;
  /** The one lane this taker may end; finding another, or none, leaves the Volume alone. */
  readonly lane?: string;
  readonly fences?: VolumeRunKind;
};

export type LaneTake = { readonly run: VolumeRunView; readonly ended: VolumeRunView | null };

const endOf = (kind: VolumeRunKind): LaneEnd | null => kind === "release" ? "cancelled" : null;

const TAKEN_BY = {
  cancelled: "volume release took this run over",
  yielded: "a Deploy took this run over",
} satisfies Record<LaneEnd, string>;

/** `owed` marks a refusal whose Server is named only after the transaction ends, so no row waits on the Cluster. */
class LaneRefused extends Data.TaggedError("LaneRefused")<{ readonly refusal: StoreRefusal; readonly busy: VolumeRunView; readonly owed?: Owed }> {}

class LaneMoved extends Data.TaggedError("LaneMoved")<{ readonly active: VolumeRunRow | undefined }> {}

class LaneChurn extends Data.TaggedError("LaneChurn")<{ readonly message: string }> {}

type Caller = { readonly userId: string | null; readonly organizationId: string };

const TAKE_ROUNDS = 3;

/**
 * The insert waits on any lane committing concurrently, so a conflict means a lane landed between the read and the
 * insert; the next round reads it under the lock and takes it over too. A lane that ended before the read leaves the
 * Volume free, so any taker inserts again.
 */
const takeLane = Effect.fn("VolumeRun.takeLane")(function* (caller: Caller, { input, volume, id, lane: only, fences }: LaneTaker, end: LaneEnd | null, inngestRunId: string | null = null) {
  const { drizzle } = yield* Database;
  for (let round = 1; ; round++) {
    const lane = end === null ? undefined : yield* activeLane(input.volumeId, true);
    if (only !== undefined && lane?.id !== only) return yield* new LaneMoved({ active: lane });
    let taken: VolumeRunRow | undefined;
    if (lane !== undefined && end !== null) {
      if (keepsLane(lane, end, input.kind)) return yield* new LaneRefused({ refusal: yield* laneRefusal(lane, input.kind), busy: volumeRunView(lane) });
      [taken] = yield* drizzle.update(volumeRun).set(ended(end, TAKEN_BY[end]))
        .where(and(eq(volumeRun.id, lane.id), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES), eq(volumeRun.phase, "warm")))
        .returning();
    }
    const [inserted] = yield* drizzle.insert(volumeRun).values({
      id,
      organizationId: caller.organizationId,
      environmentId: volume.environmentId,
      volumeId: input.volumeId,
      volumeName: volume.volumeName,
      dockerVolume: volume.dockerVolume,
      refquotaBytes: volume.refquotaBytes,
      recordedBytes: volume.recordedBytes,
      kind: input.kind,
      args: input.args,
      state: inngestRunId === null ? "requested" : "running",
      inngestRunId,
      requestedByUserId: caller.userId,
    }).onConflictDoNothing().returning();
    if (inserted !== undefined) {
      // Read after the insert, not before: the insert waited out any lane still active, so a stamped lane that ended
      // between a read and the insert is seen here.
      const [held] = input.kind === "deploy" ? [] : yield* drizzle.select().from(volumeRun)
        .where(and(eq(volumeRun.volumeId, input.volumeId), isNotNull(volumeRun.heldBy)))
        .limit(1);
      if (held !== undefined) return yield* new LaneRefused({ refusal: yield* laneRefusal(held, input.kind), busy: volumeRunView(held) });
      const admitted = yield* admitOwed(inserted, input);
      // A release floors and plans on its own; only a yielding run fences the lane it took before it plans.
      if (inngestRunId === null) yield* enqueueRun(admitted, end === "yielded" ? taken?.kind ?? fences : undefined);
      if (taken !== undefined) yield* enqueueEvent(createVolumeRunTakenEvent({ runId: taken.id }));
      return { run: volumeRunView(admitted), ended: taken === undefined ? null : volumeRunView(taken) } satisfies LaneTake;
    }
    if (end !== null && round < TAKE_ROUNDS) continue;
    const active = yield* activeLane(input.volumeId, true);
    if (active === undefined && round < TAKE_ROUNDS) continue;
    if (active === undefined) return yield* new LaneChurn({ message: `runs on ${volume.volumeName} keep landing and ending; try again` });
    return yield* new LaneRefused({ refusal: yield* laneRefusal(active, input.kind), busy: volumeRunView(active) });
  }
});

/**
 * Read after the insert, under the lane it holds: a release inherits a commitment still owed its finish, and a run
 * that would undo it is refused. A user's release must have previewed that same commitment; Cloud's carries no plan.
 * The commitment stays locked until the transaction commits, so a removal's clear waits for the release that inherits
 * it, and the removal's select then finds that release.
 */
const admitOwed = Effect.fn("VolumeRun.admitOwed")(function* (inserted: VolumeRunRow, input: VolumeLaneInput) {
  const { newest, owed: pending } = yield* commitmentHeldAgainstRemoval(inserted.volumeId);
  const plan = input.kind === "release" ? input.args.plan : undefined;
  const follows = input.kind === "release" ? input.args.follows : undefined;
  if (follows !== undefined && pending?.current !== follows) {
    return yield* new LaneRefused({ refusal: followSettled(inserted.volumeName), busy: volumeRunView(newest ?? inserted) });
  }
  if (pending === null) {
    if (plan?.forward === undefined) return inserted;
    return yield* new LaneRefused({ refusal: releaseChanged(inserted.volumeName), busy: volumeRunView(newest ?? inserted) });
  }
  if (input.kind !== "release") {
    if (!refusedWhileOwed(input, pending)) return inserted;
    return yield* new LaneRefused({ refusal: yield* owedRefusal(pending, yield* laneTarget(pending)), busy: volumeRunView(pending), owed: pending });
  }
  if (plan !== undefined && (plan.forward?.after !== pending.id || plan.forward.to !== pending.current)) {
    return yield* new LaneRefused({ refusal: releaseChanged(inserted.volumeName), busy: volumeRunView(pending) });
  }
  const { drizzle } = yield* Database;
  const [inherited] = yield* drizzle.update(volumeRun).set({ phase: "committed", current: pending.current, epoch: pending.epoch })
    .where(eq(volumeRun.id, inserted.id)).returning();
  return inherited ?? inserted;
});

/** A run admitted in its own transaction; an owed refusal still names the Server by its recorded name. */
export const admitRun = Effect.fn("VolumeRun.admit")(function* (
  caller: Caller, input: { readonly volumeId: string } & VolumeRunInput, volume: ResolvedVolume, id: string, end: LaneEnd | null = endOf(input.kind),
  fences?: VolumeRunKind,
) {
  const database = yield* Database;
  return yield* database.transaction(takeLane(caller, { input, volume, id, fences }, end)).pipe(
    Effect.map(({ run, ended }) => ({ ok: true, run, took: ended !== null }) as const),
    Effect.catchTag("LaneRefused", ({ refusal, busy, owed }) => Effect.succeed({ ok: false, refusal, busy, owed: owed ?? null } as const)),
  );
});

/** Names the Server an owed refusal finishes on as the Cluster names it now. Call it with no row locked. */
export const nameRefusal = Effect.fn("VolumeRun.nameRefusal")(function* (
  { refusal, busy, owed }: { readonly refusal: StoreRefusal; readonly busy: VolumeRunView; readonly owed: Owed | null },
) {
  return { ok: false, refusal: owed === null ? refusal : yield* namedOwedRefusal(owed), busy } as const;
});

export const insertRun = Effect.fn("VolumeRun.insert")(function* (...args: Parameters<typeof admitRun>) {
  const admitted = yield* admitRun(...args);
  return admitted.ok ? admitted : yield* nameRefusal(admitted);
});

/** A Deploy's own lane, running under `inngestRunId` from the start; a warm lane found there yields to it. */
export const takeDeployLane = Effect.fn("VolumeRun.takeDeployLane")(function* (caller: Caller, taker: LaneTaker, inngestRunId: string) {
  const { ended } = yield* takeLane(caller, taker, "yielded", inngestRunId).pipe(
    Effect.catchTag("LaneRefused", ({ refusal }) => Effect.fail(new Error(refusal.message))),
  );
  const row = yield* readRow(taker.id);
  if (row === undefined) return yield* Effect.fail(new Error(`volume run ${taker.id} vanished after its insert`));
  return { run: contextOf(row), ended };
});

/**
 * A Deploy's hold on a Volume no run holds, so nothing starts beside its Services. It takes nothing, so a lane that
 * landed since the Deploy's read fails the step, and the retry places over that lane.
 */
export const holdDeployLane = Effect.fn("VolumeRun.holdDeployLane")(function* (caller: Caller, taker: LaneTaker, inngestRunId: string) {
  yield* takeLane(caller, taker, null, inngestRunId).pipe(
    Effect.catchTag("LaneRefused", ({ refusal }) => Effect.fail(new Error(refusal.message))),
  );
});

/** The Machines the Cluster names now; none when it does not answer. */
export const namedMachines = Effect.fn("VolumeRun.namedMachines")(function* (organizationId: string) {
  const frame = yield* openSession(organizationId).pipe(
    Effect.flatMap((session) => session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead)),
    Effect.option,
  );
  return Option.match(frame, {
    onNone: (): MachineRef[] => [],
    onSome: ({ machines }) => machines.map(({ machine }) => ({ id: machine.id, name: machine.name })),
  });
}, Effect.scoped);

export const machineNamed = Effect.fn("VolumeRun.machineNamed")(function* (organizationId: string, machine: MachineId) {
  const named = (yield* namedMachines(organizationId)).find((entry) => entry.id === machine);
  return { id: machine, name: named?.name ?? null };
});

/**
 * A Server leaving: each warm lane bound for `machine`, or on a Volume whose writer copy it holds (`writes`, by Docker
 * Volume), is taken over by a release, so the Volume's Service stays where it runs. Only requests the releases. A lane
 * past its point of no return toward `machine` can finish nowhere, so it ends failed at once, naming the copy on
 * another Server to restore from.
 */
export const releaseLanesOff = Effect.fn("VolumeRun.releaseLanesOff")(function* (
  organizationId: string,
  machine: { readonly id: MachineId; readonly name: MachineName | null },
  writes: readonly string[],
) {
  const database = yield* Database;
  const lanes = yield* database.drizzle.select().from(volumeRun).where(and(
    eq(volumeRun.organizationId, organizationId),
    inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES),
    eq(volumeRun.phase, "warm"),
  ));
  const released: string[] = [];
  for (const lane of lanes) {
    const input = laneInputOf(lane);
    if (pastReturn(lane) || input.kind === "release" || input.kind === "deploy") continue;
    const bound = isDestination(input, machine);
    if (!bound && !writes.includes(lane.dockerVolume)) continue;
    const taker: LaneTaker = {
      input: { volumeId: lane.volumeId, kind: "release", args: {} },
      volume: { environmentId: lane.environmentId, volumeName: lane.volumeName, dockerVolume: lane.dockerVolume, refquotaBytes: lane.refquotaBytes, recordedBytes: lane.recordedBytes },
      id: crypto.randomUUID(),
      lane: lane.id,
    };
    const left = (reason: string, run: string | null) =>
      Effect.logWarning("A Server leaving left a run on its Volume alone.", { machine: machine.name ?? machine.id, volume: lane.volumeName, run, reason }).pipe(Effect.as(null));
    const taken = yield* database.transaction(takeLane({ userId: null, organizationId }, taker, "cancelled")).pipe(
      Effect.catchTags({
        LaneMoved: ({ active }) => left(active === undefined ? `${lane.id} ended on its own` : `${lane.id} ended and ${active.id} took the lane`, active?.id ?? null),
        LaneRefused: ({ refusal, busy }) => left(refusal.message, busy.id),
      }),
    );
    if (taken !== null) released.push(taken.run.id);
  }
  // After the takeovers: a lane that committed before its takeover could lock it was refused, and ends here.
  yield* endLanesTo(organizationId, machine);
  return released;
});

/** Every commitment to `machine`: a committed move or release naming it that has not ended done, active or not. */
const committedTo = (organizationId: string, machine: MachineId) => and(
  eq(volumeRun.organizationId, organizationId),
  inArray(volumeRun.kind, ["move", "release"]),
  eq(volumeRun.phase, "committed"),
  eq(volumeRun.current, machine),
  ne(volumeRun.state, "done"),
);

/**
 * Ends every commitment to `machine` until one read finds none, so nothing is owed a finish there. Each pass forgets
 * the ended ones a Volume at a time, since a Deploy holds one Volume's commitment while it reaches for the next and one
 * statement holding several could deadlock against it. Then one read sees active and ended commitments together, and
 * each active one found ends failed, cancelling its Inngest run; the next pass forgets what that read saw ended.
 * It terminates: a pass moves each active commitment it reads to ended and each ended one out of the predicate. A new
 * one comes only from a lane already active, failing into its release, or from admitOwed inheriting an ended one,
 * which locks it, so the read after the forget sees it.
 */
const endLanesTo = Effect.fn("VolumeRun.endLanesTo")(function* (
  organizationId: string,
  machine: { readonly id: MachineId; readonly name: MachineName | null },
) {
  const database = yield* Database;
  const committed = committedTo(organizationId, machine.id);
  for (;;) {
    const volumes = yield* database.drizzle.selectDistinct({ volumeId: volumeRun.volumeId }).from(volumeRun).where(committed);
    for (const { volumeId } of volumes) {
      yield* database.drizzle.update(volumeRun).set({ current: null })
        .where(and(committed, eq(volumeRun.volumeId, volumeId), notInArray(volumeRun.state, [...ACTIVE_VOLUME_RUN_STATES])));
    }
    const rows = yield* database.drizzle.select().from(volumeRun).where(committed);
    if (rows.length === 0) return;
    const lanes = rows.filter((row) => ACTIVE_VOLUME_RUN_STATES.some((active) => active === row.state));
    for (const lane of lanes) {
      // The message only advises; a lane left active would block the restore it names.
      const holder = yield* observeVolumeMembers(organizationId, lane.dockerVolume).pipe(
        Effect.map((members) => restoreSource(members.filter((member) => member.machine.id !== machine.id))),
        Effect.catch((error) =>
          Effect.logWarning("A removed Server's committed lane ends without naming a copy to restore from.", { volume: lane.volumeName, error }).pipe(Effect.as(undefined))),
      );
      const message = writerRemovedMessage(lane.volumeName, machine.name ?? machine.id, holder);
      yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [closed] = yield* drizzle.update(volumeRun).set(ended("failed", message))
          .where(and(eq(volumeRun.id, lane.id), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES), eq(volumeRun.current, machine.id)))
          .returning({ id: volumeRun.id });
        if (closed !== undefined) yield* enqueueEvent(createVolumeRunTakenEvent({ runId: closed.id }));
      }));
    }
  }
});

export const listVolumeRuns = Effect.fn("VolumeRun.list")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.volumeId, volumeId)))
    .orderBy(desc(volumeRun.createdAt), desc(volumeRun.id))
    .limit(50);
  return rows.map(volumeRunView);
});

/**
 * The Volume's copies as each managed Machine answers for them, for the panel to plan its offers from. With `run`, as
 * that run's observe step reads them, so a caller can refuse what the run would.
 */
export const inspectVolumeCopies = Effect.fn("VolumeRun.inspectCopies")(function* (
  organizationId: string, environment: EnvironmentRef, volumeId: string, run?: ObservedRun,
) {
  const volume = yield* resolveVolume(organizationId, environment, volumeId);
  return yield* observeVolumeMembers(organizationId, volume.dockerVolume, run);
});

/** Whether the Volume's committed Move is still owed its finish, so the Copies tab offers no move or mirror. */
export const isVolumeOwed = Effect.fn("VolumeRun.isOwed")(function* (organizationId: string, volumeId: string) {
  const { owed } = yield* commitment(volumeId, organizationId);
  return owed !== null;
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

/** A message on a run still running, such as the Server it waits on. */
export const noteRun = Effect.fn("VolumeRun.note")(function* (ctx: Pick<RunContext, "id">, inngestRunId: string, message: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumeRun).set({ message: clip(message), updatedAt: new Date() })
    .where(and(eq(volumeRun.id, ctx.id), eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.state, "running")));
});

export const refuseRun = (ctx: Pick<RunContext, "id">, inngestRunId: string, refusal: Refusal) =>
  endRun(ctx, inngestRunId, "failed", refusal.message);

export class ClusterUnreachable extends Data.TaggedError("ClusterUnreachable")<{ readonly message: string }> {}

/** Cloud could not reach the Cluster, or it sent no frame in time: it may answer later. */
export const unreachable = (error: Error) =>
  error instanceof ClusterUnreachable || (error instanceof RuntimeConnectionFailure && error.waitable);

/** Cloud holds no connection to the Organization's Servers, as once their pairing is removed. */
export class ClusterNotConnected extends Data.TaggedError("ClusterNotConnected")<{ readonly message: string }> {}

export const openSession = Effect.fn("VolumeRun.openSession")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status === "unreachable") return yield* new ClusterUnreachable({ message: "The cluster is unreachable." });
  if (session.status === "no_connection") return yield* new ClusterNotConnected({ message: "The cluster is no_connection." });
  return session.connected;
});

/** The reasons SeedVolume and the copy verbs may refuse with. */
export type SwitchReason = CopyRefusal | SwitchError["reason"];

const SWITCH_REASONS = [
  "mismatch", "no_transition", "busy", "no_capacity", "behind", "written", "no_source",
  "precondition", "volume_switching", "no_writer", "restore_point_gone", "stale",
] as const satisfies readonly SwitchReason[];

const SwitchErrorDetails = Schema.Struct({ details: Schema.Struct({ reason: Schema.Literals(SWITCH_REASONS) }) });
const decodeSwitchError = Schema.decodeUnknownOption(SwitchErrorDetails);

export function switchErrorOf(error: PloyzSdkError): SwitchReason | null {
  const decoded = decodeSwitchError("cause" in error ? error.cause : undefined);
  return Option.isSome(decoded) ? decoded.value.details.reason : null;
}

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
      return `${name} on ${machine.name} is mid-run; try again once the run ends`;
    case "no_writer":
      return `${name} has no writer on ${machine.name}`;
    case "no_capacity":
      return `${machine.name} has no room for ${name}'s mirror`;
    case "restore_point_gone":
      return `the restore point is gone from ${name} on ${machine.name}`;
    case "stale":
      return `a newer run took ${name} on ${machine.name}`;
    case "busy":
      return undefined;
  }
}

/**
 * Whether the Machine got a failed request: `answered` when it sent the failure, `unanswered` when no reply came back, so
 * it may have acted on it, and `unreached` when the Cluster's dial to it failed, so it never got the request.
 */
export type SwitchReach = "answered" | "unanswered" | "unreached";

/** A failed attempt the step may retry; `reason` is the Machine's refusal, if it gave one. */
export class SwitchAttemptError extends Error {
  constructor(readonly reason: SwitchReason | null, message: string, readonly reach: SwitchReach = "answered") {
    super(message);
  }
}

const switchReach = (cause: unknown): SwitchReach => {
  if (!(cause instanceof RpcError)) return "answered";
  if (cause.targetUnreached) return "unreached";
  return cause.unanswered ? "unanswered" : "answered";
};

/** The verbs a run still sends through `sendSwitch`: SeedVolume and the reads. */
type SwitchRequest = Extract<VolumeSwitchRequest, { command: "seed_volume" | "inspect_volume_copy" }>;

/** A refusal with a message ends the run; Busy and transport failures fail only the attempt, so the step retries. */
export const sendSwitch = <R extends SwitchRequest>(ctx: RunContext, inngestRunId: string, machine: MachineRef, request: R) => {
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
  }).pipe(Effect.scoped);
};

/** Plain JSON, since a step returns it: the copy the Machine holds now, after the step or after its refusal. */
export type CopyAnswer =
  | { readonly ok: true; readonly copy: VolumeCopy | null }
  | { readonly ok: false; readonly reason: CopyRefusal; readonly copy: VolumeCopy | null; readonly message: string };

/**
 * Who a SetVolumeCopy is for: a run, a Deploy following another run's lane without owning its row, or the rowless
 * mirror sync at the Volume's newest gen.
 */
export type CopyAsker = Pick<RunContext, "organizationId" | "dockerVolume" | "volumeName" | "gen">;

/**
 * One SetVolumeCopy. A refusal is an answer, since the caller decides what follows it; only a failure to ask fails,
 * so a run's step retries. Spent Cluster time fails it before it opens a session, and out of time is `inFlight` only
 * when the SetVolumeCopy itself was cut. Any other failure but a `SwitchAttemptError` is a session that didn't open, so
 * the SetVolumeCopy was never sent.
 */
export const askCopy = Effect.fn("VolumeRun.askCopy")(function* (asker: CopyAsker, machine: MachineRef, expect: ExpectedCopy, want: WantedCopy) {
  yield* requireClusterTime;
  const session = yield* openSession(asker.organizationId).pipe(
    Effect.catchIf((error) => error instanceof ClusterOutOfTime, () => Effect.fail(new ClusterOutOfTime({ inFlight: false }))),
  );
  return yield* session.volumeSwitch(machine.id, { command: "set_volume_copy", payload: { name: asker.dockerVolume, expect, want, gen: asker.gen } }).pipe(
    clusterRead,
    Effect.map((reply): CopyAnswer => ({ ok: true, copy: reply.copy })),
    Effect.catch((error): Effect.Effect<CopyAnswer, ClusterOutOfTime | SwitchAttemptError> => {
      if (error instanceof ClusterOutOfTime) return Effect.fail(error);
      const refused = setVolumeCopyRefusal("cause" in error ? error.cause : undefined);
      if (refused === null) {
        const message = `set_volume_copy on ${machine.name}: ${sdkFailureMessage(error, [machine])}`;
        return Effect.fail(new SwitchAttemptError(null, message, switchReach("cause" in error ? error.cause : undefined)));
      }
      const { reason, copy } = refused;
      const message = switchFailureMessage(reason, asker, machine) ?? `${machine.name} is busy with ${asker.volumeName}`;
      return Effect.succeed<CopyAnswer>({ ok: false, reason, copy, message });
    }),
  );
}, Effect.scoped);

export const setVolumeCopy = Effect.fn("VolumeRun.setVolumeCopy")(function* (
  ctx: RunContext,
  inngestRunId: string,
  machine: MachineRef,
  expect: ExpectedCopy,
  want: WantedCopy,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* askCopy(ctx, machine, expect, want);
});

export const inspectCopy = (ctx: RunContext, inngestRunId: string, machine: MachineRef) =>
  sendSwitch(ctx, inngestRunId, machine, { command: "inspect_volume_copy", payload: { name: ctx.dockerVolume } }).pipe(
    Effect.map((view) => view.copy),
  );

export type Holder = ServiceSpec & { readonly containerId: ContainerId };

/** Drain::DEFAULT: how long the cut lets requests in flight finish once the Container is out of ingress. */
export const DRAIN_MS = 3_000;

const serviceContainers = Effect.fn("VolumeRun.serviceContainers")(function* (organizationId: string, dockerVolume: string) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead);
  return frame.containers.filter((container) =>
    container.kind === "service_container"
    && container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === dockerVolume));
}, Effect.scoped);

/** The one Service container on `machine` that mounts this Volume, whose image B needs before the cut. */
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
 * The commit: this run makes `to` the writer. It wins once, and again for the run that already won it or a release that
 * inherited it; a row taken over or committed elsewhere loses.
 */
export const commitMove = Effect.fn("VolumeRun.commit")(function* (ctx: Pick<RunContext, "id">, inngestRunId: string, to: MachineId) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.update(volumeRun)
    .set({ phase: "committed", current: to, updatedAt: new Date() })
    .where(and(
      eq(volumeRun.id, ctx.id),
      eq(volumeRun.inngestRunId, inngestRunId),
      eq(volumeRun.state, "running"),
      or(eq(volumeRun.phase, "warm"), and(eq(volumeRun.phase, "committed"), eq(volumeRun.current, to))),
    ))
    .returning({ id: volumeRun.id });
  return rows.length === 1;
});

/**
 * Where B starts: `from` is the Machine whose stopped Container B copies, or null when the run does not know it, so the
 * one stopped Container on any other Machine.
 */
type WriterStart = {
  readonly to: MachineRef;
  readonly from: MachineRef | null;
  readonly expect: ExpectedCopy;
  readonly at: SnapshotGuid;
  readonly epoch: number;
};

/**
 * The point of no return: the commit, then B becomes the writer at G and creates the Service from A's stopped
 * Container. One step, so B is never told to start by a run that does not own the commit, and the spec never
 * lands in a step result. A B that already runs the Service is left alone.
 */
export const commitAndStart = Effect.fn("VolumeRun.commitAndStart")(function* (
  ctx: RunContext,
  inngestRunId: string,
  start: WriterStart,
) {
  const { to } = start;
  if (!(yield* commitMove(ctx, inngestRunId, to.id))) {
    const lost: CopyAnswer = { ok: false, reason: "stale", copy: null, message: `a newer run took ${ctx.volumeName} before ${to.name} started it` };
    return lost;
  }
  return yield* startWriter(ctx, inngestRunId, start);
});

/**
 * B becomes the writer at G and creates the Service from the source's stopped Container, at its gen and routed once
 * healthy when the source was routed; a B already running it is left alone, unless the source is routed and B is not,
 * when B is asked again and fails while its copy stays unrouted. With no source named, a B whose stopped Service is
 * the only one left becomes the writer and stays stopped: an earlier start moved the Service there, and a Deploy's
 * placement may have removed the source since. `ownedBy` is the Inngest run that must still own the row when B is
 * told, or null for a Deploy following another run's committed lane, which owns no row.
 */
export const startWriter = Effect.fn("VolumeRun.startWriter")(function* (ctx: RunContext, ownedBy: string | null, start: WriterStart) {
  const { to, from } = start;
  const containers = yield* serviceContainers(ctx.organizationId, ctx.dockerVolume);
  const stopped = containers.filter((container) =>
    container.runtime.state !== "running" && (from === null ? container.machine_id !== to.id : container.machine_id === from.id));
  const routed = stopped.some((container) => container.rotation === "in");
  if (containers.some((container) =>
    container.machine_id === to.id && container.runtime.state === "running" && (container.rotation === "in" || !routed))) {
    const running: CopyAnswer = { ok: true, copy: null };
    return running;
  }
  const [holder] = stopped;
  if (holder === undefined && from === null && containers.some((container) => container.machine_id === to.id)) {
    if (ownedBy !== null) yield* requireOwner(ctx.id, ownedBy);
    return yield* askCopy(ctx, to, start.expect, { kind: "writer", at: start.at, epoch: start.epoch, start: null });
  }
  if (holder === undefined) {
    return yield* Effect.fail(new NonRetriableError(`${ctx.volumeName} has no stopped Service container${from === null ? "" : ` on ${from.name}`} to start on ${to.name}`));
  }
  if (stopped.length > 1) {
    const named = yield* Effect.forEach(stopped, (container) => machineNamed(ctx.organizationId, container.machine_id));
    const on = from?.name ?? named.map((machine) => machine.name ?? machine.id).join(" and ");
    return yield* Effect.fail(new NonRetriableError(`${ctx.volumeName} has ${stopped.length} stopped Service containers on ${on}; starting it on ${to.name} needs exactly one`));
  }
  const session = yield* openSession(ctx.organizationId);
  const { container } = yield* session.inspectContainer(holder.machine_id, holder.container_id).pipe(
    Effect.mapError((error) => new Error(`reading ${holder.resolved_spec.name}'s spec: ${sdkFailureMessage(error)}`)),
  );
  // The spec was resolved at the Service's last Deploy; a resize since then raised the copies, and the Store recorded it.
  const volumes = container.resolved_spec.volumes.map((volume) =>
    volume.source.kind === "provisioned" && volume.source.name === ctx.dockerVolume && ctx.recordedBytes > 0
      ? { ...volume, source: { ...volume.source, maximum_bytes: ctx.recordedBytes } }
      : volume);
  const spec = { ...container.resolved_spec, volumes };
  if (ownedBy !== null) yield* requireOwner(ctx.id, ownedBy);
  return yield* askCopy(ctx, to, start.expect, {
    kind: "writer",
    at: start.at,
    epoch: start.epoch,
    start: {
      kind: "create",
      namespace: holder.namespace,
      resolved_spec: { ...spec, container: { ...spec.container, pull_policy: "never" } },
      creation_key: creationKey(ctx, start.at),
      gen: container.gen ?? 1,
      rotation: container.rotation === "in" ? { in: { run: null, expect: "pending" } } : "pending",
    },
  });
}, Effect.scoped);

/** Removes the stopped Service containers the Move left on `machines`; a rerun finds the ones already gone gone. */
export const removeKept = Effect.fn("VolumeRun.removeKept")(function* (ctx: RunContext, inngestRunId: string, machines: readonly MachineRef[]) {
  yield* requireOwner(ctx.id, inngestRunId);
  const containers = yield* serviceContainers(ctx.organizationId, ctx.dockerVolume);
  for (const container of containers) {
    const machine = machines.find((candidate) => candidate.id === container.machine_id);
    if (machine === undefined || container.runtime.state === "running") continue;
    yield* removeContainer(ctx, inngestRunId, machine, container.container_id);
  }
});

export const removeContainer = Effect.fn("VolumeRun.removeContainer")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef, containerId: ContainerId) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  yield* session.removeContainer(machine.id, containerId).pipe(
    Effect.mapError((error) => new Error(`removing ${ctx.volumeName}'s old Container on ${machine.name}: ${sdkFailureMessage(error, [machine])}`)),
  );
}, Effect.scoped);

/** A run as the observe step reads it: a resize carries the size the Store records, which decides its direction. */
export type ObservedRun = Exclude<VolumeLaneInput, { kind: "resize" }> | (Extract<VolumeLaneInput, { kind: "resize" }> & { readonly recordedBytes: number });

const storageOf = (volumes: readonly DockerVolume[], machineId: MachineId, dockerVolume: string) =>
  volumes.find((volume) => volume.id.machine_id === machineId && volume.id.name === dockerVolume)?.storage;

const boundOf = (volumes: readonly DockerVolume[], machineId: MachineId, dockerVolume: string) => {
  const storage = storageOf(volumes, machineId, dockerVolume);
  return storage?.kind === "provisioned" ? storage.bound_bytes : null;
};

/** A slot no step marked, as an older daemon made it, is a mirror. */
const lastMirror = (volumes: readonly DockerVolume[], machineId: MachineId, dockerVolume: string) => {
  const storage = storageOf(volumes, machineId, dockerVolume);
  return storage?.kind === "provisioned" && storage.role === "slot" && (storage.slot_role ?? "mirror") === "mirror";
};

/**
 * Each managed Server's copy of `dockerVolume`; a Server that doesn't answer in time is a member that didn't answer.
 * So is one the frame lists only by id: it is still in the cluster, so a mirror on it is not gone. Each that didn't
 * answer and last published a mirror there is marked, for Copies to list as last reported; nothing stages from it.
 * For a run, a Server that holds a copy or receives one and lacks the generation floor is a member that lacks it.
 */
export const observeVolumeMembers = Effect.fn("VolumeRun.observeMembers")(function* (organizationId: string, dockerVolume: string, run?: ObservedRun) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead);
  // A Docker-only Machine holds no copy and may not serve the verb, so it must not block the run.
  const managed = frame.machines.filter((observed) => observed.storage?.state !== "stateless");
  // A resize names them, so its record of a Volume no Server holds can't miss a copy a stateless report hides.
  const stateless = run?.kind === "resize"
    ? frame.machines.filter((observed) => observed.storage?.state === "stateless")
      .map(({ machine }): Member => ({ machine: { id: machine.id, name: machine.name }, answered: false, stateless: true }))
    : [];
  const unlisted = frame.incomplete_ids.machines.map((id): Member => ({ machine: { id, name: id }, answered: false, incomplete: true }));
  const listed = yield* Effect.forEach(managed, ({ machine }) => {
    const ref: MachineRef = { id: machine.id, name: machine.name };
    const server = { name: machine.name, daemonVersion: machine.runtime.daemon_version };
    const lacks = (capability: string, kind: VolumeRunKind | undefined) => lackSentence(organizationId, capability, server, kind).pipe(
      Effect.map((said): Member => ({ machine: ref, answered: false, ...said })),
    );
    /** Named by the verb, not the run, so a resize says which direction the Server lacks: "Volume shrink needs a newer Ployz on db-1." */
    const lacksVerb = (requirement: Requirement) => lacks(requirement.capability, undefined);
    const capabilities = { capabilities: machine.runtime.capabilities ?? [] };
    const floorless = run !== undefined && serverSupports(capabilities, requirements.volumeFloor) === false;
    const unsized = run?.kind === "resize"
      ? resizeRequirements(run.recordedBytes, run.args.maximum_bytes).find((requirement) => serverSupports(capabilities, requirement) !== true)
      : undefined;
    const sized = run?.kind === "resize" ? { boundBytes: boundOf(frame.volumes, machine.id, dockerVolume) } : {};
    if (run?.kind === "move" && isDestination(run, ref) && serverSupports(capabilities, movedRouting) === false) {
      return lacks(movedRouting.capability, run.kind);
    }
    return session.volumeSwitch(machine.id, { command: "inspect_volume_copy", payload: { name: dockerVolume } }).pipe(
      clusterRead,
      Effect.timeout(OBSERVE_TIMEOUT),
      Effect.flatMap((view) => floorless && needsFloor(run, ref, view.copy)
        ? lacks(requirements.volumeFloor.capability, run?.kind)
        : unsized !== undefined && view.copy !== null
        ? lacksVerb(unsized)
        : Effect.succeed<Member>({
          machine: ref,
          address: managementAddress(machine.public_key),
          answered: true,
          view,
          floorless,
          ...sized,
        })),
      Effect.catchTag("ServerLacks", (lack) => lacks(lack.capability, run?.kind)),
      Effect.orElseSucceed((): Member => ({ machine: ref, answered: false })),
    );
  }, { concurrency: "unbounded" });
  return [...listed, ...stateless, ...unlisted].map((member): Member =>
    !member.answered && lastMirror(frame.volumes, member.machine.id, dockerVolume) ? { ...member, lastMirror: true } : member);
}, Effect.scoped);

const lackSentence = Effect.fn("VolumeRun.lackSentence")(function* (
  organizationId: string, capability: string, server: { readonly name: string; readonly daemonVersion: string }, kind: VolumeRunKind | undefined,
) {
  const required = requirementFor(capability) ?? requirements.volumeMove;
  const feature: Requirement = kind === undefined ? required : { capability: required.capability, feature: RUN_FEATURES[kind] };
  const line = releaseLine(server.daemonVersion);
  const release = line === null ? null : yield* organizationReleaseChannel(organizationId).pipe(
    Effect.flatMap((channel) => channelRelease(channel, line).pipe(Effect.timeout(OBSERVE_TIMEOUT))),
    Effect.orElseSucceed(() => null),
  );
  // A newer Ployz is claimed only on proof the Server's release is behind; otherwise the sentence says only what it lacks.
  const missing = `${feature.feature} ${feature.plural ? "are" : "is"} not ${server.daemonVersion === "" ? "available" : `in Ployz ${server.daemonVersion}`} on ${server.name}`;
  if (release !== null && isBehind(server.daemonVersion, release)) {
    const needs = `${feature.feature} ${feature.plural ? "need" : "needs"} a newer Ployz on ${server.name}`;
    return { lack: `${needs}, which runs ${server.daemonVersion}. Upgrade it to ${release}.`, outdated: true as const };
  }
  return { lack: release !== null && isCurrent(server.daemonVersion, release) ? `${missing} yet.` : `${missing}.` };
});

export const observeMembers = Effect.fn("VolumeRun.observe")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* observeVolumeMembers(ctx.organizationId, ctx.dockerVolume, ctx);
});

/**
 * observeMembers, or null when Cloud could not reach the Cluster or it did not answer in time, which a forward release
 * waits out like a silent Server.
 */
export const observeReachable = Effect.fn("VolumeRun.observeReachable")(function* (ctx: RunContext, inngestRunId: string) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* observeVolumeMembers(ctx.organizationId, ctx.dockerVolume, ctx).pipe(
    Effect.catchIf(unreachable, () => Effect.succeed(null)),
  );
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

type Closing = ReturnType<typeof ended>;

/**
 * The move a Deploy took over is requested again once its row ends, in the same transaction as the end: a retry finds
 * no running row and requests nothing. A refusal is logged; the lane that refused owns the Volume now.
 */
export const requeueTakenMoves = Effect.fn("VolumeRun.requeueTakenMoves")(function* (rows: readonly VolumeRunRow[]) {
  const requeued: string[] = [];
  const refused: string[] = [];
  for (const row of rows) {
    const input = laneInputOf(row);
    if (input.kind !== "deploy") continue;
    const { resume } = input.args;
    if (resume === null) continue;
    const again = yield* admitRun(
      { userId: null, organizationId: row.organizationId },
      { volumeId: row.volumeId, kind: "move", args: resume },
      { environmentId: row.environmentId, volumeName: row.volumeName, dockerVolume: row.dockerVolume, refquotaBytes: row.refquotaBytes, recordedBytes: row.recordedBytes },
      crypto.randomUUID(),
    );
    if (again.ok) {
      requeued.push(again.run.id);
      continue;
    }
    refused.push(again.refusal.message);
    yield* Effect.logWarning(`${row.volumeName}'s move to ${resume.to} was not requested again after its Deploy: ${again.refusal.message}`);
  }
  return { requeued, refused };
});

/**
 * Ends the running row `owned` picks. A Move it ended hands the Volume to a release run: back to A while warm, forward
 * to B once committed. A release never starts another. A Deploy it ended requests the move it took over again.
 */
const endAndRelease = Effect.fn("VolumeRun.endAndRelease")(function* (owned: SQL | undefined, closing: Closing) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.update(volumeRun).set(closing).where(and(owned, eq(volumeRun.state, "running"))).returning();
    yield* requeueTakenMoves(rows);
    for (const row of rows.filter((ended) => ended.kind === "move")) {
      const [release] = yield* drizzle.insert(volumeRun).values({
        organizationId: row.organizationId,
        environmentId: row.environmentId,
        volumeId: row.volumeId,
        volumeName: row.volumeName,
        dockerVolume: row.dockerVolume,
        refquotaBytes: row.refquotaBytes,
        recordedBytes: row.recordedBytes,
        kind: "release",
        args: {},
        phase: row.phase,
        current: row.current,
        epoch: row.phase === "committed" ? row.epoch : null,
      }).onConflictDoNothing().returning();
      if (release !== undefined) yield* enqueueRun(release);
    }
    return rows.length;
  }));
});

/** A failure after the rounds: the run ends its row and releases what it left in flight. */
export const failAndRelease = (ctx: Pick<RunContext, "id">, inngestRunId: string, message: string) =>
  endAndRelease(and(eq(volumeRun.id, ctx.id), eq(volumeRun.inngestRunId, inngestRunId)), ended("failed", message)).pipe(
    Effect.andThen(Effect.fail(new NonRetriableError(clip(message)))),
  );

export const closeVolumeRun = Effect.fn("VolumeRun.close")(function* (
  inngestRunId: string,
  end: keyof typeof CLOSED_BY | { readonly error: string | null },
) {
  const closing = end === "failure" || end === "cancellation"
    ? ended(CLOSED_BY[end].state, CLOSED_BY[end].message)
    : end.error === null
      ? ended("lost", "the run ended without recording how")
      : ended("failed", end.error.trim().length > 0 ? end.error : "the run failed");
  return yield* endAndRelease(eq(volumeRun.inngestRunId, inngestRunId), closing);
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
    .set(ended("not_started", "event never delivered"))
    .where(and(eq(volumeRun.state, "requested"), lt(volumeRun.createdAt, new Date(now - VOLUME_RUN_UNCLAIMED_LIMIT_MS))))
    .returning({ id: volumeRun.id });
  const running = yield* drizzle.select({ id: volumeRun.id, inngestRunId: volumeRun.inngestRunId }).from(volumeRun)
    .where(and(eq(volumeRun.state, "running"), lt(volumeRun.updatedAt, new Date(now - VOLUME_RUN_RUNNING_CHECK_MS))));
  let unheld = 0;
  const holders = yield* drizzle.selectDistinct({ heldBy: volumeRun.heldBy }).from(volumeRun).where(isNotNull(volumeRun.heldBy));
  for (const { heldBy } of holders) {
    if (heldBy === null) continue;
    const status = yield* lookup(heldBy).pipe(Effect.option);
    if (Option.isNone(status) || status.value === "running") continue;
    unheld += (yield* drizzle.update(volumeRun).set({ heldBy: null }).where(eq(volumeRun.heldBy, heldBy)).returning({ id: volumeRun.id })).length;
  }
  let closed = 0;
  for (const row of running) {
    if (row.inngestRunId === null) continue;
    const status = yield* lookup(row.inngestRunId).pipe(Effect.option);
    if (Option.isNone(status) || status.value === "running") continue;
    const [state, message] = STALE_ENDS[status.value];
    closed += yield* endAndRelease(and(eq(volumeRun.id, row.id), eq(volumeRun.inngestRunId, row.inngestRunId)), ended(state, message));
  }
  return { not_started: unclaimed.length, unheld, closed };
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

class ClusterNotPaired extends Data.TaggedError("ClusterNotPaired")<{ readonly organizationId: string }> {}

/** Every copy the Cluster's Servers report, failing when Cloud holds no pairing to ask them through. */
export const observeOrganization = Effect.fn("VolumeRun.observeOrganization")(function* (organizationId: string) {
  const loaded = yield* loadOrganizationConnections(organizationId);
  if (loaded.kind !== "ready") return yield* new ClusterNotPaired({ organizationId });
  if (loaded.connections.length === 0) return { copies: [], unanswered: [] } satisfies CopyObservation;
  return yield* observeVolumeCopies(loaded.connections);
});

export const observeOrganizationCopies: CopyObserver = (organizationId) =>
  observeOrganization(organizationId).pipe(
    Effect.map((observed) => observed.copies),
    Effect.catchTag("ClusterNotPaired", () => Effect.succeed([])),
  );

function unheldRootNames(copies: CopyObservation["copies"], configNames: ReadonlySet<string>): string[] {
  const roots = copies.filter((copy) => copy.role !== "slot" && !configNames.has(copy.name));
  return [...new Set(roots.map((copy) => copy.name))].sort();
}

export type NameInspector = (organizationId: string, dockerVolume: string) => ReturnType<typeof observeVolumeMembers>;

const orphanDelete = (name: string) =>
  ({ kind: "delete_mirror", args: { slot: null, confirmed_name: null }, volumeName: name, orphan: true }) satisfies PlanInput;

export const findOrphans = Effect.fn("VolumeRun.findOrphans")(function* (
  organizationId: string,
  observe: CopyObserver,
  inspect: NameInspector,
) {
  const copies = yield* observe(organizationId);
  if (copies.length === 0) return [];
  const config = yield* configVolumeNames(organizationId);
  const seeds = yield* Effect.forEach(unheldRootNames(copies, config), (name) => inspect(organizationId, name).pipe(
    Effect.map((members) => (planFromCopies(orphanDelete(name), members).ok ? [name] : [])),
    Effect.catch((error) => Effect.logWarning("Orphan inspect failed.", { organizationId, name, error }).pipe(Effect.as([]))),
  ), { concurrency: 4 });
  return [...orphanSlotNames(copies, config), ...seeds.flat()].sort();
});

export const startOrphanDeletes = Effect.fn("VolumeRun.startOrphanDeletes")(function* (
  organizationId: string,
  observe: CopyObserver = observeOrganizationCopies,
  inspect: NameInspector = observeVolumeMembers,
) {
  const database = yield* Database;
  const names = yield* findOrphans(organizationId, observe, inspect);
  const started: string[] = [];
  for (const name of names) {
    const parsed = parseDockerVolumeName(name);
    if (parsed === null) continue;
    const { kind, args, orphan } = orphanDelete(name);
    const row = yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [inserted] = yield* drizzle.insert(volumeRun).values({
      organizationId,
      environmentId: parsed.namespace,
      volumeId: parsed.volumeId,
      volumeName: name,
      dockerVolume: name,
      kind,
      args,
      orphan,
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

export const cleanOrphans = (organizationId: string) =>
  startOrphanDeletes(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Orphan cleanup failed.", { organizationId, error }).pipe(Effect.as(NO_RUNS))),
  );

export const organizationsWithMachines = Effect.fn("VolumeRun.organizationsWithMachines")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinct({ organizationId: organizationMachine.organizationId }).from(organizationMachine);
  return rows.map((row) => row.organizationId);
});
