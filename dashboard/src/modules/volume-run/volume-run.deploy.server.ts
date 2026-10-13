import "@tanstack/react-start/server-only";
import type { MachineName } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { NonRetriableError } from "inngest";
import { expectOf, observedEpoch } from "#/modules/volume-run/plan";
import { volumeRun } from "#/modules/volume-run/tables";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, type Member, type VolumeRunArgs, type VolumeRunKind } from "#/modules/volume-run/volume-run";
import {
  contextOf,
  endRun,
  insertRun,
  keptOn,
  laneTarget,
  type MachineRef,
  observeVolumeMembers,
  pastReturn,
  type RunContext,
  sendSetCopy,
  setVolumeCopy,
  startWriter,
  takeDeployLane,
  takeEpoch,
} from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

/** Plain JSON: a step result. A took lane is the Deploy's own row; a followed lane is the other run's row, read once. */
export type DeployLane =
  | { readonly kind: "took"; readonly run: RunContext; readonly resume: VolumeRunArgs["move"] | null }
  | { readonly kind: "follow"; readonly run: RunContext; readonly to: MachineName; readonly epoch: number | null };

const CLOUD = (organizationId: string) => ({ userId: null, organizationId });

/** A lane that never touches the writer; the Deploy runs beside it. */
const PLAIN: ReadonlySet<VolumeRunKind> = new Set<VolumeRunKind>(["mirror", "sync", "seed", "delete_mirror"]);

const FENCE_ROUNDS = 3;

/**
 * D1. One transaction over the Environment's lanes, each locked: a warm move, release or another Deploy yields to this
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
      if (PLAIN.has(lane.kind)) continue;
      if (lane.kind === "deploy" && lane.inngestRunId === inngestRunId) {
        // SAFETY: a deploy row's args were written from a VolumeRunArgs["deploy"].
        placed.push({ kind: "took", run: contextOf(lane), resume: (lane.args as VolumeRunArgs["deploy"]).resume });
        continue;
      }
      if (pastReturn(lane)) {
        const to = yield* laneTarget(lane);
        if (to === null) return yield* Effect.fail(new NonRetriableError(`${lane.volumeName} is moving to a Server its run never named`));
        placed.push({ kind: "follow", run: contextOf(lane), to, epoch: lane.epoch });
        continue;
      }
      // SAFETY: a move row's args are a Destination.
      const resume = lane.kind === "move" ? (lane.args as VolumeRunArgs["move"]) : null;
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

const answered = (members: readonly Member[]) => members.filter((member): member is AnsweredMember => member.answered);

// SAFETY: a took lane is the Deploy's own row, written from a VolumeRunArgs["deploy"].
const observeMembers = (run: RunContext) => observeVolumeMembers(run.organizationId, run.dockerVolume, { kind: "deploy", args: run.args as VolumeRunArgs["deploy"] });

/** The Server a took lane fences: the root, else the slot that kept the stopped Service after a cut. */
const writerOf = Effect.fn("VolumeRun.writerOf")(function* (run: RunContext, members: readonly AnsweredMember[]) {
  const root = members.find((member) => member.view.copy?.kind === "root");
  if (root !== undefined) return root;
  const slots = members.filter((member) => member.view.copy?.kind === "slot");
  const kept = yield* keptOn(run.organizationId, run.dockerVolume, slots.map((member) => member.machine));
  const held = kept === null ? undefined : slots.find((member) => member.machine.id === kept.machine.id);
  if (held === undefined) return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no writer and no stopped Service on its copies`));
  return held;
});

/**
 * D2 and D3 for a took lane. The Floor on A with the Deploy's gen makes every older run's verb stale there. A root reply
 * is the writer; a slot reply is a cut that finished before the floor rose, promoted back to the writer with the next
 * epoch before any Service mounts it.
 */
const fenceTaken = Effect.fn("VolumeRun.fenceTaken")(function* (run: RunContext, inngestRunId: string) {
  for (let round = 1; ; round++) {
    const members = yield* observeMembers(run);
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
    if (copy?.kind !== "slot" || copy.newest === null) {
      return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no copy to deploy on ${writer.machine.name}`));
    }
    const { epoch } = yield* takeEpoch(run, inngestRunId, observedEpoch(replied));
    const promoted = yield* setVolumeCopy(run, inngestRunId, writer.machine, expectOf(copy), { kind: "writer", at: copy.newest.guid, epoch, start: null });
    if (!promoted.ok) return yield* Effect.fail(retriable(promoted.reason) ? new Error(promoted.message) : new NonRetriableError(promoted.message));
    return { on: writer.machine.name };
  }
});

const retriable = (reason: string) => reason === "busy" || reason === "mismatch";

/** D2 for a followed lane: the other run's idempotent Writer on B, with its gen, so the Deploy runs where the Volume now lives. */
const fenceFollowed = Effect.fn("VolumeRun.fenceFollowed")(function* (lane: Extract<DeployLane, { kind: "follow" }>) {
  const { run, to } = lane;
  const members = answered(yield* observeVolumeMembers(run.organizationId, run.dockerVolume));
  const b = members.find((member) => member.machine.name === to);
  if (b === undefined) return yield* Effect.fail(new Error(`${to} did not answer for ${run.volumeName}`));
  const copy = b.view.copy;
  if (copy?.kind === "root" && run.kind === "restore") return { on: to };
  if (copy === null || copy.newest === null) return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no copy on ${to} to deploy`));
  const epoch = copy.kind === "root" ? copy.epoch : lane.epoch;
  if (epoch === null) return yield* Effect.fail(new Error(`${run.volumeName}'s run on ${to} has not chosen an epoch yet`));
  const machine: MachineRef = b.machine;
  const started = run.kind === "restore"
    ? yield* sendSetCopy(run, machine, expectOf(copy), { kind: "writer", at: copy.newest.guid, epoch, start: null })
    : yield* startWriter(run, { to: machine, expect: expectOf(copy), at: copy.newest.guid, epoch });
  if (!started.ok) return yield* Effect.fail(retriable(started.reason) ? new Error(started.message) : new NonRetriableError(started.message));
  return { on: to };
});

export const fenceDeployVolume = (lane: DeployLane, inngestRunId: string) =>
  lane.kind === "took" ? fenceTaken(lane.run, inngestRunId) : fenceFollowed(lane);

/** D4. The Deploy's own rows end done; a move it took over is requested again, once, by the attempt that ended the row. */
export const finishDeployVolumes = Effect.fn("VolumeRun.finishDeploy")(function* (lanes: readonly DeployLane[], environmentId: string, inngestRunId: string) {
  let ended = 0;
  const requeued: string[] = [];
  const refused: string[] = [];
  for (const lane of lanes) {
    if (lane.kind !== "took") continue;
    const { run, resume } = lane;
    const rows = yield* endRun(run, inngestRunId, "done", null);
    ended += rows;
    if (rows !== 1 || resume === null) continue;
    const again = yield* insertRun(
      CLOUD(run.organizationId),
      { volumeId: run.volumeId, kind: "move", args: resume },
      { environmentId, volumeName: run.volumeName, dockerVolume: run.dockerVolume, refquotaBytes: run.refquotaBytes },
      crypto.randomUUID(),
    );
    if (again.ok) requeued.push(again.run.id);
    else refused.push(again.refusal.message);
  }
  return { ended, requeued, refused };
});
