import "@tanstack/react-start/server-only";
import type { MachineId, MachineName } from "@ployz/sdk";
import { and, eq, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import { NonRetriableError } from "inngest";
import { readStore } from "#/modules/config-store/config-store.server";
import { expectOf, observedEpoch, oldCopy, restoreLine, restoreSource } from "#/modules/volume-run/plan";
import { volumeRun } from "#/modules/volume-run/tables";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, copyName, dockerVolumeName, type Member, type VolumeRunArgs, type VolumeRunKind } from "#/modules/volume-run/volume-run";
import {
  askCopy,
  commitment,
  contextOf,
  type CopyAnswer,
  ended,
  holdDeployLane,
  insertRun,
  keptOn,
  laneInputOf,
  laneTarget,
  type MachineRef,
  observeMembers,
  observeVolumeMembers,
  owedCommitments,
  pastReturn,
  requeueTakenMoves,
  type RunContext,
  setVolumeCopy,
  startWriter,
  SwitchAttemptError,
  takeDeployLane,
  takeEpoch,
} from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";
import { uuidFrom } from "#/server/uuid-from";

/**
 * Plain JSON: a step result. A took lane is the Deploy's own row; a followed lane is the other run's row, read once.
 * `machine` is the Server the lane committed to, by id, so a rename since cannot redirect the Deploy; null follows `to`
 * by name. `ended` marks a commitment no run holds, which only a release the Deploy requests settles.
 */
export type DeployLane =
  | { readonly kind: "took"; readonly run: RunContext; readonly resume: VolumeRunArgs["move"] | null }
  | {
    readonly kind: "follow";
    readonly run: RunContext;
    readonly to: MachineName;
    readonly machine: MachineId | null;
    readonly epoch: number | null;
    readonly ended: boolean;
  };

const CLOUD = (organizationId: string) => ({ userId: null, organizationId });

// A sync may demote the writer with a cut, so it is not beside the writer.
const BESIDE_WRITER: ReadonlySet<VolumeRunKind> = new Set<VolumeRunKind>(["mirror", "seed", "delete_mirror"]);

const FENCE_ROUNDS = 3;

/**
 * One transaction over the Environment's lanes, each locked: a warm move, release or another Deploy yields to this
 * Deploy's own row; a lane past its return, or a committed Move no run holds any more, is followed to its Server; the
 * rest are plain. A row already running under this Inngest run is the step's own earlier attempt. Each Volume the
 * Deploy targets that no run holds gets a hold, and a lane on one that the Deploy neither takes nor holds is stamped
 * `held_by`, so a Move asked for while the Services run is refused even after that lane ends; a hold took nothing, so
 * it is neither placed nor fenced.
 */
export const placeDeployVolumes = Effect.fn("VolumeRun.placeDeploy")(function* (
  organizationId: string,
  environmentId: string,
  deploymentId: string,
  inngestRunId: string,
) {
  const deployment = yield* readStore(organizationId, { query: "deployment", id: deploymentId });
  const { volumes } = yield* readStore(organizationId, {
    query: "volumes",
    environment: { project: deployment.environment.project, environment: deployment.environment.name },
  });
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const lanes = yield* drizzle.select().from(volumeRun)
      .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.environmentId, environmentId), inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES)))
      .orderBy(volumeRun.volumeId)
      .for("update");
    const targets = new Set(deployment.nodes.flatMap((node) => (node.type === "volume" ? [node.id] : [])));
    const beside: string[] = [];
    const placed: DeployLane[] = [];
    for (const lane of lanes) {
      if (BESIDE_WRITER.has(lane.kind)) {
        if (targets.has(lane.volumeId)) beside.push(lane.id);
        continue;
      }
      const input = laneInputOf(lane);
      const deploy = input.kind === "deploy" ? input.args : null;
      if (deploy !== null && lane.inngestRunId === inngestRunId) {
        if (deploy.hold !== true) placed.push({ kind: "took", run: contextOf(lane), resume: deploy.resume });
        continue;
      }
      if (pastReturn(lane)) {
        const to = yield* laneTarget(lane);
        if (to === null) return yield* Effect.fail(new NonRetriableError(`${lane.volumeName} is moving to a Server its run never named`));
        placed.push({ kind: "follow", run: contextOf(lane), to, machine: lane.current ?? pinned(lane), epoch: lane.epoch, ended: false });
        if (targets.has(lane.volumeId)) beside.push(lane.id);
        continue;
      }
      const hold = deploy?.hold === true;
      const resume = input.kind === "move" ? input.args : (deploy?.resume ?? null);
      const { run } = yield* takeDeployLane(CLOUD(organizationId), {
        input: { volumeId: lane.volumeId, kind: "deploy", args: hold ? { deployment: deploymentId, resume: null, hold } : { deployment: deploymentId, resume } },
        volume: { environmentId, volumeName: lane.volumeName, dockerVolume: lane.dockerVolume, refquotaBytes: lane.refquotaBytes },
        id: crypto.randomUUID(),
      }, inngestRunId);
      if (!hold) placed.push({ kind: "took", run, resume });
    }
    if (beside.length > 0) yield* drizzle.update(volumeRun).set({ heldBy: inngestRunId }).where(inArray(volumeRun.id, beside));
    const laned = new Set(lanes.map((lane) => lane.volumeId));
    for (const id of targets) {
      const volume = laned.has(id) ? undefined : volumes.find((entry) => entry.id === id);
      if (volume === undefined) continue;
      yield* holdDeployLane(CLOUD(organizationId), {
        input: { volumeId: volume.id, kind: "deploy", args: { deployment: deploymentId, resume: null, hold: true } },
        volume: {
          environmentId,
          volumeName: volume.name,
          dockerVolume: dockerVolumeName(deployment.namespace, volume.id),
          refquotaBytes: volume.storage.kind === "provisioned" ? volume.storage.maximumBytes : 0,
        },
        id: crypto.randomUUID(),
      }, inngestRunId);
    }
    for (const row of yield* owedCommitments(organizationId, environmentId)) {
      // A hold, its own or one it took over, takes nothing, so the commitment still has no run.
      if (lanes.some((lane) => lane.volumeId === row.volumeId && !BESIDE_WRITER.has(lane.kind) && !isHold(lane))) continue;
      const to = yield* laneTarget(row);
      if (to === null) return yield* Effect.fail(new NonRetriableError(`${row.volumeName} is moving to a Server its run never named`));
      placed.push({ kind: "follow", run: contextOf(row), to, machine: row.current, epoch: row.epoch, ended: true });
    }
    return placed;
  }));
});

const isHold = (lane: Pick<typeof volumeRun.$inferSelect, "kind" | "args">) => {
  const input = laneInputOf(lane);
  return input.kind === "deploy" && input.args.hold === true;
};

const pinned = (lane: Pick<typeof volumeRun.$inferSelect, "kind" | "args">) => {
  const input = laneInputOf(lane);
  return input.kind === "move" ? input.args.machine ?? null : null;
};

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
 * Floors every answered Machine at this run's gen, the empty ones too, so the run it took can switch nothing more
 * anywhere, not even with a late Mirror onto its destination. A Server without the floor answers only when it holds no
 * copy, so it stays as observed. It may be the destination of the run this one took, but a Machine with no copy
 * refuses a writer, so a late request leaves at most a slot there. A Machine that refuses or fails its floor stops
 * none of the others: they are floored first, then the first failure is the answer, since the run may end before it
 * fences again.
 */
const floorMembers = Effect.fn("VolumeRun.floorMembers")(function* (run: RunContext, inngestRunId: string, members: readonly AnsweredMember[]) {
  const floored: AnsweredMember[] = [];
  let failed: Extract<CopyAnswer, { ok: false }> | SwitchAttemptError | null = null;
  for (const member of members) {
    if (member.floorless) {
      floored.push(member);
      continue;
    }
    const answer = yield* setVolumeCopy(run, inngestRunId, member.machine, expectOf(member.view.copy), { kind: "floor" }).pipe(
      Effect.catchIf((error) => error instanceof SwitchAttemptError, (error) => Effect.succeed(error)),
    );
    if (answer instanceof SwitchAttemptError || !answer.ok) failed ??= answer;
    else floored.push({ ...member, view: { ...member.view, copy: answer.copy } });
  }
  if (failed instanceof SwitchAttemptError) return yield* Effect.fail(failed);
  return failed ?? ({ ok: true, members: floored } as const);
});

/**
 * D2 and D3 of a takeover: the run floors every copy, so the lane it took can switch nothing more, and a cut that
 * flipped A before the floor rose is promoted back to the writer at its snapshot, starting the Service the cut stopped.
 * The Deploy's own lanes and a staged placement's run both fence this way before they plan.
 */
export const fenceTaken = Effect.fn("VolumeRun.fenceTaken")(function* (run: RunContext, inngestRunId: string) {
  for (let round = 1; ; round++) {
    const members = yield* observeMembers(run, inngestRunId);
    for (const member of members) if (!member.answered && member.lack !== undefined) return yield* Effect.fail(new NonRetriableError(member.lack));
    const floored = yield* floorMembers(run, inngestRunId, answered(members));
    if (!floored.ok) {
      if (floored.reason === "mismatch" && round < FENCE_ROUNDS) continue;
      return yield* Effect.fail(retriable(floored.reason) ? new Error(floored.message) : new NonRetriableError(floored.message));
    }
    const writer = yield* writerOf(run, floored.members);
    const copy = writer.view.copy;
    if (copy?.kind === "root") return { on: writer.machine.name };
    // Read after the floor: a cut can flip a root it observed before the floor rose, and stops nothing on A after it.
    const kept = copy?.kind === "slot" ? yield* keptOn(run.organizationId, run.dockerVolume, [writer.machine]) : null;
    if (copy?.kind !== "slot" || copy.newest === null || kept === null) {
      return yield* Effect.fail(new NonRetriableError(`${run.volumeName} has no copy to deploy on ${writer.machine.name}`));
    }
    // Core refuses a plain mount of a slot, so the promote lands before any Service mounts the Volume. It starts the
    // cut's stopped Container too: the taken lane is not requested again, so if this run stops before its own start,
    // nothing else would.
    const { epoch } = yield* takeEpoch(run, inngestRunId, observedEpoch(floored.members));
    const start = { kind: "kept", container_id: kept.containerId } as const;
    const promoted = yield* setVolumeCopy(run, inngestRunId, writer.machine, expectOf(copy), { kind: "writer", at: copy.newest.guid, epoch, start });
    if (!promoted.ok) return yield* Effect.fail(retriable(promoted.reason) ? new Error(promoted.message) : new NonRetriableError(promoted.message));
    return { on: writer.machine.name };
  }
});

const retriable = (reason: string) => reason === "busy" || reason === "mismatch";

const fenceFollowed = Effect.fn("VolumeRun.fenceFollowed")(function* (lane: Extract<DeployLane, { kind: "follow" }>) {
  const { run, to } = lane;
  if (run.kind !== "restore") {
    const { newest } = yield* commitment(run.volumeId);
    const successor = newest !== undefined && newest.kind !== "restore" && newest.current === run.current && newest.epoch === lane.epoch;
    if (newest !== undefined && newest.gen > run.gen && !successor) {
      return yield* Effect.fail(new NonRetriableError(`${run.volumeName} changed since this Deploy placed it; Deploy again`));
    }
  }
  const members = yield* observeVolumeMembers(run.organizationId, run.dockerVolume);
  const isB = (member: Member) => (lane.machine === null ? member.machine.name === to : member.machine.id === lane.machine);
  const restore = () => restoreLine(run.volumeName, restoreSource(members.filter((member) => !isB(member))));
  const b = answered(members).find(isB);
  if (b === undefined) {
    return yield* Effect.fail(new Error(`${run.volumeName} finishes on ${to}, which did not answer. Bring ${to} back or remove it with ployz server rm ${to}` + restore()));
  }
  const machine: MachineRef = b.machine;
  const copy = b.view.copy;
  if (copy?.kind === "root" && run.kind === "restore") return { on: machine.name };
  if (copy === null || copy.newest === null) {
    return yield* Effect.fail(new NonRetriableError(`${run.volumeName} finishes on ${machine.name}, which holds no copy to start. Remove it with ployz server rm ${machine.name}` + restore()));
  }
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

/**
 * After the Deploy ran, each ended commitment it followed gets a Cloud release, which inherits it and settles it on B.
 * The ID is the Deploy's and the Volume's, so a retried step finds its own release and requests none. A refusal is
 * logged: the lane that refused owns the Volume now.
 */
export const releaseFollowed = Effect.fn("VolumeRun.releaseFollowed")(function* (
  environmentId: string,
  inngestRunId: string,
  lanes: readonly DeployLane[],
) {
  const requested: string[] = [];
  for (const lane of lanes) {
    if (lane.kind !== "follow" || !lane.ended) continue;
    const { run } = lane;
    const id = uuidFrom(`ployz.deploy-release:${inngestRunId}:${run.volumeId}`);
    const { owed } = yield* commitment(run.volumeId);
    if (owed === null || owed.id === id) continue;
    const release = yield* insertRun(
      CLOUD(run.organizationId),
      { volumeId: run.volumeId, kind: "release", args: {} },
      { environmentId, volumeName: run.volumeName, dockerVolume: run.dockerVolume, refquotaBytes: run.refquotaBytes },
      id,
    );
    if (release.ok) {
      requested.push(release.run.id);
      continue;
    }
    yield* Effect.logWarning(`${run.volumeName}'s release on ${lane.to} was not requested after its Deploy: ${release.refusal.message}`);
  }
  return requested;
});

const DEPLOY_ENDS = { done: null, failed: "the Deploy failed", cancelled: "the Deploy was cancelled" } as const;

/**
 * The Deploy's own rows end, whichever way the Deploy did, and a move it took over is requested again, once. Its holds
 * are deleted in any state, so a Volume's history keeps no trace of them, and its stamps are cleared first, so they
 * refuse none of those moves.
 */
export const finishDeployVolumes = Effect.fn("VolumeRun.finishDeploy")(function* (inngestRunId: string, state: keyof typeof DEPLOY_ENDS) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const held = sql`${volumeRun.args}->>'hold' = 'true'`;
    yield* drizzle.delete(volumeRun).where(and(eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.kind, "deploy"), held));
    yield* drizzle.update(volumeRun).set({ heldBy: null }).where(eq(volumeRun.heldBy, inngestRunId));
    const rows = yield* drizzle.update(volumeRun).set(ended(state, DEPLOY_ENDS[state]))
      .where(and(eq(volumeRun.inngestRunId, inngestRunId), eq(volumeRun.kind, "deploy"), eq(volumeRun.state, "running")))
      .returning();
    const { requeued, refused } = yield* requeueTakenMoves(rows);
    return { ended: rows.length, requeued, refused };
  }));
});
