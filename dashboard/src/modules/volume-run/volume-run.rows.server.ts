import "@tanstack/react-start/server-only";
import type { DestructiveEffect, EnvironmentRef, MachineId, SnapshotGuid, VolumeKind } from "@ployz/sdk";
import { and, desc, eq, inArray, isNotNull, isNull, lt, max, ne, or, type SQL, sql } from "drizzle-orm";
import { Clock, Data, Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import type { OperationVerb } from "#/modules/approvals/approvals";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import { serversUnanswered, type StoreRefusal } from "#/lib/refusal";
import type { JsonObject, JsonValue } from "#/db/tables";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { volumeRunRequested, volumeRunTaken } from "#/modules/volume-run/volume-run.events";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { ClusterOutOfTime, clusterRead, firstRuntimeFrame, OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { newestGuid, type Planned, planForwardRelease, type Refusal, type RefusalCode, type ReleaseFrom } from "#/modules/volume-run/plan";
import { type DeploymentsSeen, deployingMessage, gb } from "#/modules/volume-run/resize";
import { volumeRun } from "#/modules/volume-run/tables";
import { originBlockers, originUpgradeMessage } from "#/modules/runtime/daemon-release";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, copyName, type MachineRef, type Member, namesServer, VOLUME_RUN_MESSAGE_LIMIT, VOLUME_RUN_RUNNING_CHECK_MS, VOLUME_RUN_UNCLAIMED_LIMIT_MS, type VolumeLaneInput, type VolumeRunArgs, type VolumeRunInput, type VolumeRunView } from "#/modules/volume-run/volume-run";
import { dockerVolumeName, parseDockerVolumeName } from "#/modules/runtime/naming";
import { type VolumeRunKind, type VolumeRunPhase, type VolumeRunState } from "#/modules/volume-run/volume-run-states";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { uuidFrom } from "#/server/uuid-from";
import { frameServers } from "#/modules/runtime/daemon-compat";

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
  /** The snapshot the commit made `current` the writer at. Absent from a context a step memoized before Cloud kept it. */
  readonly at?: SnapshotGuid | null;
};

/** Plain JSON: Inngest memoizes step output as JSON. */
export type RunContext = RunBase & VolumeLaneInput;


export const OBSERVE_TIMEOUT = "10 seconds";

/**
 * The gen a rowless SetVolumeCopy carries: the Volume's newest lane's. With no lane open, every floor Cloud raised on
 * this Volume is at most that, and a lane opened since raises the floor past it, so the Machine refuses the ask Stale.
 * A Deploy's hold floors no Machine and its row goes when the Deploy ends, so an ask carrying its gen would raise a
 * floor no later ask reaches.
 */
export const rowlessGen = Effect.fn("VolumeRun.rowlessGen")(function* (volumeId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ gen: max(volumeRun.gen) }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, volumeId), sql`${volumeRun.args}->>'hold' is distinct from 'true'`));
  return row?.gen ?? 0;
});

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
  at: row.at,
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
export const keepsLane = (lane: VolumeRunRow, end: LaneEnd, kind: VolumeRunKind) =>
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

export const activeLane = Effect.fn("VolumeRun.activeLane")(function* (volumeId: string, lock = false) {
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
    // The Environment went between the two reads: the run keeps the name it recorded.
    Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(undefined)),
  );
  return named ?? run.volumeName;
});

/** Names the Volume as it is called at the refusal, so the hints run as printed. */
export const laneRefusal = Effect.fn("VolumeRun.laneRefusal")(function* (lane: VolumeRunRow, kind: VolumeRunKind) {
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

export type Owed = VolumeRunRow & { readonly current: MachineId };

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
export const refusedWhileOwed = (input: VolumeLaneInput, pending: Owed) => {
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

export const namedOwedRefusal = Effect.fn("VolumeRun.namedOwedRefusal")(function* (pending: Owed) {
  const { name } = yield* machineNamed(pending.organizationId, pending.current);
  return yield* owedRefusal(pending, name ?? (yield* laneTarget(pending)));
});

type DeleteMirrorArgs = VolumeRunArgs["delete_mirror"];

export const withSlotMachine = (args: DeleteMirrorArgs, machines: readonly MachineRef[]): DeleteMirrorArgs => {
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

export const refusedAsSettled = Schema.is(Schema.Struct({ details: Schema.Struct({ reason: Schema.Literal("follow_settled") }) }));

export const enqueueRun = (row: VolumeRunRow, took?: VolumeRunKind) =>
  enqueueEvent(volumeRunRequested.create({
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

export type ResolvedVolume = Omit<Effect.Success<ReturnType<typeof resolveVolume>>, "environment" | "born">;

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
    born: volume.born === true,
  };
});

/**
 * The storage a Volume holds: the size the Store recorded for a deployed Managed Volume, which a staged older revision
 * leaves as it is, else what Working State asks.
 */
export const heldStorage = (volume: { readonly storage: VolumeKind; readonly recorded_storage?: number }): VolumeKind =>
  volume.recorded_storage === undefined ? volume.storage : { kind: "provisioned", maximumBytes: volume.recorded_storage };

/** A seed copies the source's dataset as is, so the restored Volume must still hold the source's storage. */
export const seedLandingRefusal = Effect.fn("VolumeRun.seedLandingRefusal")(function* (
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

/**
 * A plan's refusal as the Store hears it; a call whose outcome already holds is settled, so the Store offers no retry.
 */
export const storeRefusal = ({ code, message, next }: Refusal): StoreRefusal => {
  if (code === "already_on") return { code: refusalCode[code], message, details: { settled: true } };
  return { code: refusalCode[code], message, details: next === undefined ? null : { next } };
};

export const deletesCopy = (volume: ResolvedVolume, member: AnsweredMember): DestructiveEffect => ({
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
export function releasePlan(from: ReleaseFrom | null, source: Option.Option<MachineId | null>): PlannedRelease {
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
export function phaseEffects(volume: ResolvedVolume, phase: PlannedPhase, forgets: readonly AnsweredMember[]): DestructiveEffect[] {
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

export const VERBS = {
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
export const PROTECTED_VERBS = {
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
export function requestPreview(volume: ResolvedVolume, input: VolumeRunInput, members: readonly Member[]): JsonObject {
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
export function requestName(volume: ResolvedVolume, input: VolumeRunInput): string {
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

export type PlanRequest = { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput;

/**
 * `input` is the request to insert: a Release carries the plan its approval showed. `guard` is the ask a Protected
 * Environment makes of someone who can't protect it, when the run interrupts what runs there.
 */
export type ClusterPlan =
  | { readonly ok: true; readonly volume: ResolvedVolume; readonly asked: OperationAsked | null; readonly guard: OperationAsked | null; readonly input: PlanRequest }
  | { readonly ok: false; readonly refusal: StoreRefusal; readonly effects?: readonly DestructiveEffect[] };

export type VolumeRunPlan =
  | (Extract<ClusterPlan, { ok: true }> & { readonly environment: { readonly project: string; readonly name: string } })
  | Extract<ClusterPlan, { ok: false }>;

export const unanswered = serversUnanswered("unavailable");

/**
 * Which of `servers` can't hold a born Volume: their Ployz, or the Ployz of a Server that holds a copy of `dockerVolume`
 * now, can't check where a copy's data began. A Server the frame doesn't know is named by its id. `named` is a Server
 * the caller names rather than knows by id; one the frame lacks is no Server of this cluster.
 */
export const readOrigins = Effect.fn("VolumeRun.readOrigins")(function* (
  organizationId: string, dockerVolume: string | null, servers: readonly string[], named: VolumeRunArgs["mirror"] | null = null,
) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  if (frame === null) return { kind: "unanswered" } as const;
  const machines = frameServers(frame);
  const target = named === null ? undefined : machines.find((machine) => namesServer(named, machine));
  if (named !== null && target === undefined) return { kind: "unknown", server: named.to } as const;
  const holders = frame.volumes.filter((entry) => entry.id.name === dockerVolume).map((entry) => entry.id.machine_id);
  return { kind: "read", blockers: originBlockers(machines, [...target === undefined ? [] : [target.id], ...servers, ...holders]) } as const;
});

/**
 * Why a born Volume can't be held by `servers`, as `readOrigins` finds it, in the words of a refusal. `unanswered` is the
 * refusal while no Server answers. Null when every one can.
 */
export const originRefusal = Effect.fn("VolumeRun.originRefusal")(function* (
  organizationId: string,
  volume: { readonly volumeName: string; readonly dockerVolume: string | null },
  servers: readonly string[],
  unanswered: StoreRefusal,
  named: VolumeRunArgs["mirror"] | null = null,
) {
  const origins = yield* readOrigins(organizationId, volume.dockerVolume, servers, named);
  if (origins.kind === "unanswered") return unanswered;
  if (origins.kind === "unknown") return { code: "invalid_argument", message: `${origins.server} is not a Server of this cluster`, details: null } satisfies StoreRefusal;
  if (origins.blockers.length === 0) return null;
  return { code: "upgrade_required", message: originUpgradeMessage(volume.volumeName, origins.blockers), details: { servers: origins.blockers } } satisfies StoreRefusal;
});

/**
 * A shrink asked while a Deployment of the Environment is in flight, refused before any row in the words the Store
 * refuses its record in. The Store's refusal is the authority; this one only spares a run that would fail at its record.
 */
export const shrinkWhileDeploying = Effect.fn("VolumeRun.shrinkWhileDeploying")(function* (
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
export const deploymentsSeen = Effect.fn("VolumeRun.deploymentsSeen")(function* (organizationId: string, environment: EnvironmentRef) {
  const readAt = Math.floor((yield* Clock.currentTimeMillis) / 1000);
  const { latest, in_flight: inFlight } = yield* readStore(organizationId, { query: "deployments", environment, limit: 1, cursor: null });
  const [running] = inFlight;
  return { latest: latest?.number ?? null, inFlight: running === undefined ? null : { number: running.number }, readAt } satisfies DeploymentsSeen;
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
export const answered = <A, E, R>(read: Effect.Effect<A, E, R>) => read.pipe(
  Effect.map(Option.some),
  Effect.catch((error) => error instanceof ClusterOutOfTime ? Effect.fail(error) : Effect.succeedNone),
);

export const owedTarget = Effect.fn("VolumeRun.owedTarget")(function* (pending: Owed, members: readonly Member[]) {
  return members.find((member) => member.machine.id === pending.current)?.machine
    ?? { id: pending.current, name: (yield* laneTarget(pending)) ?? pending.current };
});

/**
 * A release that finishes a committed Move on its new Server, pinned to the commitment and the Servers it previewed.
 */
export const planForward = Effect.fn("VolumeRun.planForward")(function* (volume: ResolvedVolume, input: PlanRequest, pending: Owed, members: readonly Member[]) {
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
  const { born, ...volume } = yield* resolveVolume(caller.organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(caller.organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal, busy: null } as const;
  }
  if ((input.kind === "mirror" || input.kind === "move") && born) {
    const refusal = yield* originRefusal(caller.organizationId, volume, [], unanswered, input.args);
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
export const takeLane = Effect.fn("VolumeRun.takeLane")(function* (caller: Caller, { input, volume, id, lane: only, fences }: LaneTaker, end: LaneEnd | null, inngestRunId: string | null = null) {
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
      if (taken !== undefined) yield* enqueueEvent(volumeRunTaken.create({ runId: taken.id }));
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
  const previewed = plan?.forward?.after === pending.id && plan.forward.to === pending.current;
  if (plan !== undefined && !previewed) {
    return yield* new LaneRefused({ refusal: releaseChanged(inserted.volumeName), busy: volumeRunView(pending) });
  }
  const { drizzle } = yield* Database;
  const [inherited] = yield* drizzle.update(volumeRun).set({ phase: "committed", current: pending.current, at: pending.at, epoch: pending.epoch })
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

class GuardRefused extends Data.TaggedError("GuardRefused")<{ readonly refusal: StoreRefusal }> {}

/**
 * A run admitted under `guard`, both in one transaction: `guard` reads and locks whatever the caller's own row needs.
 * It runs again once the insert holds the lane, since the insert may wait on another lane's commit, and a refusal
 * either time, or the lane's, rolls the insert and its event back. A drain admits its moves this way, under its own
 * row. Takes no lane.
 */
export const admitGuardedRun = Effect.fn("VolumeRun.admitGuarded")(function* <E, R>(
  caller: Caller,
  input: { readonly volumeId: string } & VolumeRunInput,
  volume: ResolvedVolume,
  id: string,
  guard: Effect.Effect<StoreRefusal | null, E, R>,
) {
  const database = yield* Database;
  const guarded = guard.pipe(Effect.flatMap((refusal) => refusal === null ? Effect.void : Effect.fail(new GuardRefused({ refusal }))));
  return yield* database.transaction(Effect.gen(function* () {
    yield* guarded;
    const { run } = yield* takeLane(caller, { input, volume, id }, null);
    yield* guarded;
    return { ok: true, run } as const;
  })).pipe(
    Effect.catchIf((error): error is LaneRefused | GuardRefused => error instanceof LaneRefused || error instanceof GuardRefused, (error) =>
      Effect.succeed({ ok: false, refusal: error.refusal, owed: error instanceof LaneRefused ? error.owed ?? null : null } as const)),
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

/** Every commitment to `machine`: a committed move or release naming it that has not ended done, active or not. */
export const committedTo = (organizationId: string, machine: MachineId) => and(
  eq(volumeRun.organizationId, organizationId),
  inArray(volumeRun.kind, ["move", "release"]),
  eq(volumeRun.phase, "committed"),
  eq(volumeRun.current, machine),
  ne(volumeRun.state, "done"),
);

export const listVolumeRuns = Effect.fn("VolumeRun.list")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeRun)
    .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.volumeId, volumeId)))
    .orderBy(desc(volumeRun.createdAt), desc(volumeRun.id))
    .limit(50);
  return rows.map(volumeRunView);
});

/** Whether the Volume's committed Move is still owed its finish, so the Copies tab offers no move or mirror. */
export const isVolumeOwed = Effect.fn("VolumeRun.isOwed")(function* (organizationId: string, volumeId: string) {
  const { owed } = yield* commitment(volumeId, organizationId);
  return owed !== null;
});

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

export const requireOwner = Effect.fn("VolumeRun.requireOwner")(function* (rowId: string, inngestRunId: string, lock = false) {
  const { drizzle } = yield* Database;
  const owner = drizzle.select({ id: volumeRun.id }).from(volumeRun)
    .where(and(eq(volumeRun.id, rowId), eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.state, "running")));
  const [owned] = yield* (lock ? owner.for("update") : owner);
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

export const newestFirst = <C extends { readonly created_at_unix_nanos: number }>(containers: readonly C[]) =>
  [...containers].sort((a, b) => b.created_at_unix_nanos - a.created_at_unix_nanos);

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

/** The Release a Move's end queues, by the Move's id, so whoever started the Move finds it without a column. */
export const releaseOfMove = (moveId: string) => uuidFrom(`ployz.move-release:${moveId}`);

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
        id: releaseOfMove(row.id),
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
        at: row.phase === "committed" ? row.at : null,
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
