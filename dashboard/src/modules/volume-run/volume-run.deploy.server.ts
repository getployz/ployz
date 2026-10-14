import "@tanstack/react-start/server-only";
import type { MachineId, MachineName } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { NonRetriableError } from "inngest";
import { expectOf, observedEpoch, oldCopy } from "#/modules/volume-run/plan";
import { volumeRun } from "#/modules/volume-run/tables";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, copyName, type Member, type VolumeRunArgs, type VolumeRunKind } from "#/modules/volume-run/volume-run";
import {
  askCopy,
  contextOf,
  ended,
  keptOn,
  laneTarget,
  type MachineRef,
  observeMembers,
  observeVolumeMembers,
  pastReturn,
  requeueTakenMoves,
  type RunContext,
  setVolumeCopy,
  startWriter,
  takeDeployLane,
  takeEpoch,
} from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

/**
 * Plain JSON: a step result. A took lane is the Deploy's own row; a followed lane is the other run's row, read once.
 * `machine` is the Server the lane committed to, by id, so a rename since cannot redirect the Deploy; null follows `to`
 * by name.
 */
export type DeployLane =
  | { readonly kind: "took"; readonly run: RunContext; readonly resume: VolumeRunArgs["move"] | null }
  | { readonly kind: "follow"; readonly run: RunContext; readonly to: MachineName; readonly machine: MachineId | null; readonly epoch: number | null };

const CLOUD = (organizationId: string) => ({ userId: null, organizationId });

// A sync may demote the writer with a cut, so it is not beside the writer.
const BESIDE_WRITER: ReadonlySet<VolumeRunKind> = new Set<VolumeRunKind>(["mirror", "seed", "delete_mirror"]);

const FENCE_ROUNDS = 3;

/**
 * One transaction over the Environment's lanes, each locked: a warm move, release or another Deploy yields to this
 * Deploy's own row; a lane past its return is followed to its Server; the rest are plain. A row already running under
 * this Inngest run is the step's own earlier attempt.
 */
export const placeDeployVolumes = Effect.fn("VolumeRun.placeDeploy")(function* (
  organizationId: string,
  environmentId: string,
  deploymentId: string,
  inngestRunId: string,
) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const lanes = yield* drizzle.select().from(volumeRun)
      .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.environmentId, environmentId), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES)))
      .orderBy(volumeRun.volumeId)
      .for("update");
    const placed: DeployLane[] = [];
    for (const lane of lanes) {
      if (BESIDE_WRITER.has(lane.kind)) continue;
      if (lane.kind === "deploy" && lane.inngestRunId === inngestRunId) {
        // SAFETY: a deploy row's args were written from a VolumeRunArgs["deploy"].
        placed.push({ kind: "took", run: contextOf(lane), resume: (lane.args as VolumeRunArgs["deploy"]).resume });
        continue;
      }
      if (pastReturn(lane)) {
        const to = yield* laneTarget(lane);
        if (to === null) return yield* Effect.fail(new NonRetriableError(`${lane.volumeName} is moving to a Server its run never named`));
        placed.push({ kind: "follow", run: contextOf(lane), to, machine: lane.current ?? pinned(lane), epoch: lane.epoch });
        continue;
      }
      // SAFETY: a move row's args are a Destination; a deploy row's were written from a VolumeRunArgs["deploy"].
      const resume = lane.kind === "move"
        ? (lane.args as VolumeRunArgs["move"])
        : lane.kind === "deploy" ? (lane.args as VolumeRunArgs["deploy"]).resume : null;
      const { run } = yield* takeDeployLane(CLOUD(organizationId), {
        input: { volumeId: lane.volumeId, kind: "deploy", args: { deployment: deploymentId, resume } },
        volume: { environmentId, volumeName: lane.volumeName, dockerVolume: lane.dockerVolume, refquotaBytes: lane.refquotaBytes },
        id: crypto.randomUUID(),
      }, inngestRunId);
      placed.push({ kind: "took", run, resume });
    }
    return placed;
  }));
});

// SAFETY: a move row's args are a Destination.
const pinned = (lane: Pick<typeof volumeRun.$inferSelect, "kind" | "args">) => (lane.kind === "move" ? (lane.args as VolumeRunArgs["move"]).machine ?? null : null);

const answered = (members: readonly Member[]) => members.filter((member): member is AnsweredMember => member.answered);

/**
 * The writer, or the slot a cut left keeping the stopped Service. Of two roots it is the one a sync's demote keeps, so
 * the Deploy never writes on the root the next sync cuts.
 */
const writerOf = Effect.fn("VolumeRun.writerOf")(function* (run: RunContext, members: readonly AnsweredMember[]) {
  const roots = members.filter((member) => member.view.copy?.kind === "root");
  if (roots.length > 1) {
    const writer = oldCopy(roots)?.writer;
    if (writer === undefined) {
      const named = roots.map((member) => copyName(run.volumeName, member.machine.name)).join(" and ");
      return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has two writers, ${named}`));
    }
    return writer;
  }
  const [root] = roots;
  if (root !== undefined) return root;
  const slots = members.filter((member) => member.view.copy?.kind === "slot");
  const kept = yield* keptOn(run.organizationId, run.dockerVolume, slots.map((member) => member.machine));
  const held = kept === null ? undefined : slots.find((member) => member.machine.id === kept.machine.id);
  if (held === undefined) return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no writer and no stopped Service on its copies`));
  return held;
});

/**
 * D2 and D3 of a takeover: the run's first call to A raises its floor, so the lane it took can switch nothing more, and a
 * cut that flipped A before the floor rose is promoted back to the writer at its snapshot, starting the Service the cut
 * stopped. The Deploy's own lanes and a staged placement's run both fence this way before they plan.
 */
export const fenceTaken = Effect.fn("VolumeRun.fenceTaken")(function* (run: RunContext, inngestRunId: string) {
  for (let round = 1; ; round++) {
    const members = yield* observeMembers(run, inngestRunId);
    for (const member of members) if (!member.answered && member.lack !== undefined) return yield* Effect.fail(new NonRetriableError(member.lack));
    const replied = answered(members);
    const writer = yield* writerOf(run, replied);
    const floored = yield* setVolumeCopy(run, inngestRunId, writer.machine, expectOf(writer.view.copy), { kind: "floor" });
    if (!floored.ok) {
      if (floored.reason === "mismatch" && round < FENCE_ROUNDS) continue;
      return yield* Effect.fail(retriable(floored.reason) ? new Error(floored.message) : new NonRetriableError(floored.message));
    }
    const copy = floored.copy;
    if (copy?.kind === "root") return { on: writer.machine.name };
    // Read after the floor: a cut can flip a root it observed before the floor rose, and stops nothing on A after it.
    const kept = copy?.kind === "slot" ? yield* keptOn(run.organizationId, run.dockerVolume, [writer.machine]) : null;
    if (copy?.kind !== "slot" || copy.newest === null || kept === null) {
      return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no copy to deploy on ${writer.machine.name}`));
    }
    // Core refuses a plain mount of a slot, so the promote lands before any Service mounts the Volume. It starts the
    // cut's stopped Container too: the taken lane is not requested again, so if this run stops before its own start,
    // nothing else would.
    const { epoch } = yield* takeEpoch(run, inngestRunId, observedEpoch(replied));
    const start = { kind: "kept", container_id: kept.containerId } as const;
    const promoted = yield* setVolumeCopy(run, inngestRunId, writer.machine, expectOf(copy), { kind: "writer", at: copy.newest.guid, epoch, start });
    if (!promoted.ok) return yield* Effect.fail(retriable(promoted.reason) ? new Error(promoted.message) : new NonRetriableError(promoted.message));
    return { on: writer.machine.name };
  }
});

const retriable = (reason: string) => reason === "busy" || reason === "mismatch";

const fenceFollowed = Effect.fn("VolumeRun.fenceFollowed")(function* (lane: Extract<DeployLane, { kind: "follow" }>) {
  const { run, to } = lane;
  const members = answered(yield* observeVolumeMembers(run.organizationId, run.dockerVolume));
  const b = members.find((member) => (lane.machine === null ? member.machine.name === to : member.machine.id === lane.machine));
  if (b === undefined) return yield* Effect.fail(new Error(`${to} did not answer for ${run.volumeName}`));
  const machine: MachineRef = b.machine;
  const copy = b.view.copy;
  if (copy?.kind === "root" && run.kind === "restore") return { on: machine.name };
  if (copy === null || copy.newest === null) return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no copy on ${machine.name} to deploy`));
  const epoch = copy.kind === "root" ? copy.epoch : lane.epoch;
  if (epoch === null) return yield* Effect.fail(new Error(`${run.volumeName}'s run on ${machine.name} has not chosen an epoch yet`));
  const started = run.kind === "restore"
    ? yield* askCopy(run, machine, expectOf(copy), { kind: "writer", at: copy.newest.guid, epoch, start: null })
    : yield* startWriter(run, null, { to: machine, from: null, expect: expectOf(copy), at: copy.newest.guid, epoch });
  if (!started.ok) return yield* Effect.fail(retriable(started.reason) ? new Error(started.message) : new NonRetriableError(started.message));
  return { on: machine.name };
});

export const fenceDeployVolume = (lane: DeployLane, inngestRunId: string) =>
  lane.kind === "took" ? fenceTaken(lane.run, inngestRunId) : fenceFollowed(lane);

const DEPLOY_ENDS = { done: null, failed: "the Deploy failed", cancelled: "the Deploy was cancelled" } as const;

/** The Deploy's own rows end, whichever way the Deploy did, and a move it took over is requested again, once. */
export const finishDeployVolumes = Effect.fn("VolumeRun.finishDeploy")(function* (inngestRunId: string, state: keyof typeof DEPLOY_ENDS) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.update(volumeRun).set(ended(state, DEPLOY_ENDS[state]))
      .where(and(eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.kind, "deploy"), eq(volumeRun.state, "running")))
      .returning();
    const { requeued, refused } = yield* requeueTakenMoves(rows);
    return { ended: rows.length, requeued, refused };
  }));
});
