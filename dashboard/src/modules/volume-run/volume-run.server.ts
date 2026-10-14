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
import { and, desc, eq, inArray, isNull, lt, max, or, type SQL, sql } from "drizzle-orm";
import { Data, Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import type { OperationVerb } from "#/modules/approvals/approvals";
import { readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { JsonValue } from "#/db/tables";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { createVolumeRunRequestedEvent, createVolumeRunTakenEvent } from "#/modules/inngest/events";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { organizationMachine } from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { type Requirement, requirementFor, requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { observeVolumeCopies, type PloyzSdkError, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { alreadyOn, newestGuid, type PlanInput, type Planned, planFromCopies, type Refusal, type RefusalCode } from "#/modules/volume-run/plan";
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
  type VolumeRunArgs,
  type VolumeLaneInput,
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
  readonly gen: number;
  readonly phase: VolumeRunPhase;
  readonly current: MachineId | null;
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

export const contextOf = (row: VolumeRunRow): RunContext => ({
  id: row.id,
  organizationId: row.organizationId,
  volumeId: row.volumeId,
  volumeName: row.volumeName,
  dockerVolume: row.dockerVolume,
  kind: row.kind,
  args: row.args,
  orphan: row.orphan,
  refquotaBytes: row.refquotaBytes,
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
  deploy: "deploy",
} satisfies Record<VolumeRunKind, string>;

// A Machine may already be destroying the copy, and no floor retracts that, so a release cannot promise to stop it.
export const volumeBusyMessage = (lane: Pick<VolumeRunRow, "kind" | "volumeName">) =>
  `VolumeBusy: ${lane.volumeName} is busy with a ${KIND_WORDS[lane.kind]}; ${lane.kind === "delete_mirror" ? "it finishes on its own" : `ployz volume release ${lane.volumeName} stops it`}`;

export const finishesThere = (volumeName: string, server: string) => `${volumeName} is starting on ${server}; it finishes there`;

/** A lane past its point of no return: a Move that committed, or a restore, which commits from its first step. */
export const pastReturn = (lane: Pick<VolumeRunRow, "phase" | "kind">) => lane.phase === "committed" || lane.kind === "restore";
type LaneEnd = "cancelled" | "yielded";
// A seed's copy into the new Volume runs on without its row, so a release could only stop watching it.
const keepsLane = (lane: VolumeRunRow, end: LaneEnd) => pastReturn(lane) || ((lane.kind === "deploy" || lane.kind === "seed") && end === "cancelled");

// SAFETY: the row's kind and args were written together from one VolumeRunInput.
const laneInput = (lane: Pick<VolumeRunRow, "kind" | "args">) => ({ kind: lane.kind, args: lane.args }) as VolumeRunInput;

export const laneTarget = Effect.fn("VolumeRun.laneTarget")(function* (lane: Pick<VolumeRunRow, "volumeId" | "kind" | "args" | "current">) {
  const input = laneInput(lane);
  if (input.kind === "restore") return input.args.from;
  if (input.kind === "move") return input.args.to;
  const { current } = lane;
  if (current === null) return null;
  const { drizzle } = yield* Database;
  const [move] = yield* drizzle.select({ args: volumeRun.args }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, lane.volumeId), eq(volumeRun.kind, "move"), eq(volumeRun.current, current)))
    .orderBy(desc(volumeRun.createdAt))
    .limit(1);
  // SAFETY: a move row's args are a Destination.
  return (move?.args as VolumeRunArgs["move"] | undefined)?.to ?? null;
});

const activeLane = Effect.fn("VolumeRun.activeLane")(function* (volumeId: string, lock = false) {
  const { drizzle } = yield* Database;
  const query = drizzle.select().from(volumeRun).where(and(eq(volumeRun.volumeId, volumeId), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES)));
  const [lane] = lock ? yield* query.for("update") : yield* query;
  return lane;
});

const laneRefusal = Effect.fn("VolumeRun.laneRefusal")(function* (lane: VolumeRunRow, kind: VolumeRunKind) {
  const run = volumeRunView(lane);
  if (lane.kind === "deploy") return { code: "conflict", message: `${lane.volumeName} is deploying; the Deploy ends its run`, details: { run } } satisfies StoreRefusal;
  const input = laneInput(lane);
  if (input.kind === "seed") {
    const copying = `${lane.volumeName} is being copied into ${input.args.into.name}; it finishes on its own`;
    return { code: "conflict", message: kind === "release" ? copying : `VolumeBusy: ${copying}`, details: { run } } satisfies StoreRefusal;
  }
  if (!pastReturn(lane)) return { code: "conflict", message: volumeBusyMessage(lane), details: { run } } satisfies StoreRefusal;
  const target = yield* laneTarget(lane);
  const finishes = finishesThere(lane.volumeName, target ?? "its new Server");
  if (kind === "release") {
    // A restore's target is the copy it restores from, not a move's destination.
    const details: JsonValue = target === null || lane.kind === "restore" ? { run } : { run, move_to: target };
    return { code: "conflict", message: finishes, details } satisfies StoreRefusal;
  }
  return { code: "conflict", message: `VolumeBusy: ${finishes}`, details: { run } } satisfies StoreRefusal;
});

const enqueueRun = (row: VolumeRunRow, took?: VolumeRunKind) =>
  enqueueEvent(createVolumeRunRequestedEvent({
    organizationId: row.organizationId,
    environment: row.environmentId,
    volumeId: row.volumeId,
    runId: row.id,
    took,
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
  deploy: null,
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
  const lane = yield* activeLane(input.volumeId);
  if (lane !== undefined && (input.kind !== "release" || keepsLane(lane, "cancelled"))) {
    return { ok: false, refusal: yield* laneRefusal(lane, input.kind) } satisfies VolumeRunPlan;
  }
  const members = yield* observeVolumeMembers(organizationId, volume.dockerVolume, input).pipe(Effect.option);
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

export type RequestOptions = {
  readonly id?: string;
  /** `yielded` takes over a warm lane as a Deploy or a staged placement does; a release always cancels. */
  readonly end?: "yielded";
};

export const requestVolumeRun = Effect.fn("VolumeRun.request")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
  options: RequestOptions = {},
) {
  const database = yield* Database;
  const volume = yield* resolveVolume(caller.organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(caller.organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal, busy: null } as const;
  }
  return yield* database.transaction(insertRun(caller, input, volume, options.id ?? crypto.randomUUID(), options.end ?? endOf(input.kind)));
});

export type LaneTaker = {
  readonly input: { readonly volumeId: string } & VolumeLaneInput;
  readonly volume: ResolvedVolume;
  readonly id: string;
  /** The one lane this taker may end; finding another, or none, leaves the Volume alone. */
  readonly lane?: string;
};

export type LaneTake = { readonly run: VolumeRunView; readonly ended: VolumeRunView | null };

const endOf = (kind: VolumeRunKind): LaneEnd | null => kind === "release" ? "cancelled" : null;

const TAKEN_BY = {
  cancelled: "volume release took this run over",
  yielded: "a Deploy took this run over",
} satisfies Record<LaneEnd, string>;

class LaneRefused extends Data.TaggedError("LaneRefused")<{ readonly refusal: StoreRefusal; readonly busy: VolumeRunView }> {}

class LaneMoved extends Data.TaggedError("LaneMoved")<{ readonly active: VolumeRunRow | undefined }> {}

class LaneChurn extends Data.TaggedError("LaneChurn")<{ readonly message: string }> {}

type Caller = { readonly userId: string | null; readonly organizationId: string };

const TAKE_ROUNDS = 3;

/**
 * The insert waits on any lane committing concurrently, so a conflict means a lane landed between the read and the
 * insert; the next round reads it under the lock and takes it over too. A lane that ended before the read leaves the
 * Volume free, so any taker inserts again.
 */
const takeLane = Effect.fn("VolumeRun.takeLane")(function* (caller: Caller, { input, volume, id, lane: only }: LaneTaker, end: LaneEnd | null, inngestRunId: string | null = null) {
  const { drizzle } = yield* Database;
  for (let round = 1; ; round++) {
    const lane = end === null ? undefined : yield* activeLane(input.volumeId, true);
    if (only !== undefined && lane?.id !== only) return yield* new LaneMoved({ active: lane });
    let taken: VolumeRunRow | undefined;
    if (lane !== undefined && end !== null) {
      if (keepsLane(lane, end)) return yield* new LaneRefused({ refusal: yield* laneRefusal(lane, input.kind), busy: volumeRunView(lane) });
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
      kind: input.kind,
      args: input.args,
      state: inngestRunId === null ? "requested" : "running",
      inngestRunId,
      requestedByUserId: caller.userId,
    }).onConflictDoNothing().returning();
    if (inserted !== undefined) {
      // A release floors and plans on its own; only a yielding run fences the lane it took before it plans.
      if (inngestRunId === null) yield* enqueueRun(inserted, end === "yielded" ? taken?.kind : undefined);
      if (taken !== undefined) yield* enqueueEvent(createVolumeRunTakenEvent({ runId: taken.id }));
      return { run: volumeRunView(inserted), ended: taken === undefined ? null : volumeRunView(taken) } satisfies LaneTake;
    }
    if (end !== null && round < TAKE_ROUNDS) continue;
    const active = yield* activeLane(input.volumeId, true);
    if (active === undefined && round < TAKE_ROUNDS) continue;
    if (active === undefined) return yield* new LaneChurn({ message: `runs on ${volume.volumeName} keep landing and ending; try again` });
    return yield* new LaneRefused({ refusal: yield* laneRefusal(active, input.kind), busy: volumeRunView(active) });
  }
});

const refused = (error: LaneRefused) => Effect.succeed({ ok: false, refusal: error.refusal, busy: error.busy } as const);

export const insertRun = Effect.fn("VolumeRun.insert")(function* (
  caller: Caller, input: { readonly volumeId: string } & VolumeRunInput, volume: ResolvedVolume, id: string, end: LaneEnd | null = endOf(input.kind),
) {
  const database = yield* Database;
  const taken = yield* database.transaction(takeLane(caller, { input, volume, id }, end)).pipe(Effect.catchTag("LaneRefused", refused));
  return "ok" in taken ? taken : { ok: true, run: taken.run } as const;
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

export const machineNamed = Effect.fn("VolumeRun.machineNamed")(function* (organizationId: string, machine: MachineId) {
  const frame = yield* openSession(organizationId).pipe(
    Effect.flatMap((session) => session.watchFirstFrame(5_000)),
    Effect.option,
  );
  const name = Option.flatMap(frame, ({ machines }) => Option.fromNullishOr(machines.find((entry) => entry.machine.id === machine)?.machine.name));
  return { id: machine, name: Option.getOrNull(name) };
}, Effect.scoped);

/**
 * A Server leaving: each warm lane bound for `machine`, or on a Volume whose writer copy it holds (`writes`, by Docker
 * Volume), is taken over by a release, so the Volume's Service stays where it runs. Only requests the releases.
 */
export const releaseLanesOff = Effect.fn("VolumeRun.releaseLanesOff")(function* (
  organizationId: string, machine: { readonly id: MachineId; readonly name: MachineName | null }, writes: readonly string[],
) {
  const database = yield* Database;
  const lanes = yield* database.drizzle.select().from(volumeRun).where(and(
    eq(volumeRun.organizationId, organizationId),
    inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES),
    eq(volumeRun.phase, "warm"),
  ));
  const released: string[] = [];
  for (const lane of lanes) {
    const input = laneInput(lane);
    if (pastReturn(lane) || input.kind === "release" || lane.kind === "deploy") continue;
    const bound = machine.name !== null && isDestination(input, { id: machine.id, name: machine.name });
    if (!bound && !writes.includes(lane.dockerVolume)) continue;
    const taker: LaneTaker = {
      input: { volumeId: lane.volumeId, kind: "release", args: {} },
      volume: { environmentId: lane.environmentId, volumeName: lane.volumeName, dockerVolume: lane.dockerVolume, refquotaBytes: lane.refquotaBytes },
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
  return released;
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
  "precondition", "volume_switching", "no_writer", "restore_point_gone", "stale",
] as const satisfies readonly SwitchReason[];

const SwitchErrorDetails = Schema.Struct({ details: Schema.Struct({ reason: Schema.Literals(SWITCH_REASONS) }) });
const decodeSwitchError = Schema.decodeUnknownOption(SwitchErrorDetails);

export function switchErrorOf(error: PloyzSdkError): SwitchReason | null {
  const decoded = decodeSwitchError("cause" in error ? error.cause : undefined);
  return Option.isSome(decoded) ? decoded.value.details.reason : null;
}

const CopyRefusedDetails = Schema.Struct({
  details: Schema.Struct({
    reason: Schema.Literals(["mismatch", "no_transition", "busy", "no_capacity", "behind", "written", "no_source", "stale"]),
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
      return `${name} is mid-run; ployz volume release ${name} stops it`;
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
 * Who a SetVolumeCopy is for: a run, a Deploy following another run's lane without owning its row, or the rowless
 * mirror sync at the Volume's newest gen.
 */
export type CopyAsker = Pick<RunContext, "organizationId" | "dockerVolume" | "volumeName" | "gen">;

/**
 * One SetVolumeCopy. A refusal is an answer, since the caller decides what follows it; only a failure to ask fails,
 * so a run's step retries.
 */
export const askCopy = Effect.fn("VolumeRun.askCopy")(function* (asker: CopyAsker, machine: MachineRef, expect: ExpectedCopy, want: WantedCopy) {
  const session = yield* openSession(asker.organizationId);
  return yield* session.volumeSwitch(machine.id, { command: "set_volume_copy", payload: { name: asker.dockerVolume, expect, want, gen: asker.gen } }).pipe(
    Effect.map((reply): CopyAnswer => ({ ok: true, copy: reply.copy })),
    Effect.catch((error) => {
      const refused = decodeCopyRefused("cause" in error ? error.cause : undefined);
      if (Option.isNone(refused)) {
        return Effect.fail(new SwitchAttemptError(null, `set_volume_copy on ${machine.name}: ${sdkFailureMessage(error, [machine])}`));
      }
      const { reason } = refused.value.details;
      // SAFETY: the Machine sends the copy it holds, as the SDK's VolumeCopy.
      const copy = refused.value.details.copy as VolumeCopy | null;
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
  const frame = yield* session.watchFirstFrame(5_000);
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
 * B becomes the writer at G and creates the Service from the source's stopped Container; a B already running it is left
 * alone. `ownedBy` is the Inngest run that must still own the row when B is told, or null for a Deploy following
 * another run's committed lane, which owns no row.
 */
export const startWriter = Effect.fn("VolumeRun.startWriter")(function* (ctx: RunContext, ownedBy: string | null, start: WriterStart) {
  const { to, from } = start;
  const containers = yield* serviceContainers(ctx.organizationId, ctx.dockerVolume);
  if (containers.some((container) => container.machine_id === to.id && container.runtime.state === "running")) {
    const running: CopyAnswer = { ok: true, copy: null };
    return running;
  }
  const stopped = containers.filter((container) =>
    container.runtime.state !== "running" && (from === null ? container.machine_id !== to.id : container.machine_id === from.id));
  const [holder] = stopped;
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
  const spec = container.resolved_spec;
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

const isDestination = (run: VolumeLaneInput, machine: MachineRef) =>
  (run.kind === "move" || run.kind === "mirror") && (run.args.machine === undefined ? run.args.to === machine.name : run.args.machine === machine.id);

/**
 * Each managed Server's copy of `dockerVolume`; a Server that doesn't answer in time is a member that didn't answer.
 * For a run, a Server that holds a copy or receives one and lacks the generation floor is a member that lacks it.
 */
export const observeVolumeMembers = Effect.fn("VolumeRun.observeMembers")(function* (organizationId: string, dockerVolume: string, run?: VolumeLaneInput) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(5_000);
  // A Docker-only Machine holds no copy and may not serve the verb, so it must not block the run.
  const managed = frame.machines.filter((observed) => observed.storage?.state !== "stateless");
  return yield* Effect.forEach(managed, ({ machine }) => {
    const ref: MachineRef = { id: machine.id, name: machine.name };
    const server = { name: machine.name, daemonVersion: machine.runtime.daemon_version };
    const lacks = (capability: string) => lackSentence(organizationId, capability, server, run?.kind).pipe(
      Effect.map((lack): Member => ({ machine: ref, answered: false, lack })),
    );
    const floorless = run !== undefined && serverSupports({ capabilities: machine.runtime.capabilities ?? [] }, requirements.volumeFloor) === false;
    return session.volumeSwitch(machine.id, { command: "inspect_volume_copy", payload: { name: dockerVolume } }).pipe(
      Effect.flatMap((view) => floorless && (view.copy !== null || isDestination(run, ref))
        ? lacks(requirements.volumeFloor.capability)
        : Effect.succeed<Member>({
          machine: ref,
          address: managementAddress(machine.public_key),
          answered: true,
          view,
        })),
      Effect.timeout(OBSERVE_TIMEOUT),
      Effect.catchTag("ServerLacks", (lack) => lacks(lack.capability)),
      Effect.orElseSucceed((): Member => ({ machine: ref, answered: false })),
    );
  }, { concurrency: "unbounded" });
}, Effect.scoped);

/** What a run of each kind is called when a Server can't run it. */
const RUN_FEATURES = {
  mirror: "Volume mirror", sync: "Mirror refresh", delete_mirror: "Mirror removal", move: "Volume move", release: "Volume move", restore: "Using a mirror", seed: "Volume restore", deploy: "Deploy",
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
  return yield* observeVolumeMembers(ctx.organizationId, ctx.dockerVolume, laneInput(ctx));
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
    if (row.kind !== "deploy") continue;
    // SAFETY: a deploy row's args were written from a VolumeRunArgs["deploy"].
    const { resume } = row.args as VolumeRunArgs["deploy"];
    if (resume === null) continue;
    const again = yield* insertRun(
      { userId: null, organizationId: row.organizationId },
      { volumeId: row.volumeId, kind: "move", args: resume },
      { environmentId: row.environmentId, volumeName: row.volumeName, dockerVolume: row.dockerVolume, refquotaBytes: row.refquotaBytes },
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
  let closed = 0;
  for (const row of running) {
    if (row.inngestRunId === null) continue;
    const status = yield* lookup(row.inngestRunId).pipe(Effect.option);
    if (Option.isNone(status) || status.value === "running") continue;
    const [state, message] = STALE_ENDS[status.value];
    closed += yield* endAndRelease(and(eq(volumeRun.id, row.id), eq(volumeRun.inngestRunId, row.inngestRunId)), ended(state, message));
  }
  return { not_started: unclaimed.length, closed };
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
