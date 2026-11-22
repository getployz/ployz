import "@tanstack/react-start/server-only";
import type { ContainerId, CopyRefusal, DockerVolume, EnvironmentRef, ExpectedCopy, MachineId, MachineName, ResolvedServiceSpec, SnapshotGuid, SwitchError, VolumeCopy, VolumeSwitchRequest, WantedCopy } from "@ployz/sdk";
import { and, eq, inArray, notInArray, or } from "drizzle-orm";
import { Context, Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import type { OperationVerb } from "#/modules/approvals/approvals";
import { volumeRunTaken } from "#/modules/volume-run/volume-run.events";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { ClusterOutOfTime, clusterRead, PLAN_CLUSTER_BUDGET_MS, requireClusterTime, RUNTIME_FRAME_TIMEOUT_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { movedRouting, type Requirement, requirementFor, requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { type PloyzSdkError, RpcError, sdkFailureMessage, setVolumeCopyRefusal } from "#/modules/runtime/ployz.server";
import { alreadyOn, holdingNewest, type PlanInput, planForwardRelease, planFromCopies, restoreSource, roleOf, writerRemovedMessage } from "#/modules/volume-run/plan";
import { resizeRequirements, sizeDirection } from "#/modules/volume-run/resize";
import { isBehind, isCurrent, releaseLine } from "#/modules/server-upgrade/server-upgrade";
import { channelRelease, organizationReleaseChannel } from "#/modules/server-upgrade/server-upgrade.server";
import { volumeRun } from "#/modules/volume-run/tables";
import { checksOrigins } from "#/modules/runtime/daemon-release";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, isDestination, type Member, needsFloor, RUN_FEATURES, type ServiceSpec, type VolumeLaneInput } from "#/modules/volume-run/volume-run";
import { managementAddress } from "#/modules/runtime/naming";
import { type VolumeRunKind } from "#/modules/volume-run/volume-run-states";
import { Database } from "#/server/database.server";
import { type ClusterPlan, type LaneTaker, type MachineRef, OBSERVE_TIMEOUT, type Owed, PROTECTED_VERBS, type PlanRequest, type ResolvedVolume, type RunContext, VERBS, type VolumeRunPlan, activeLane, answered, commitment, committedTo, deletesCopy, deploymentsSeen, ended, failRun, keepsLane, laneInputOf, laneRefusal, machineNamed, namedOwedRefusal, newestFirst, openSession, originRefusal, owedTarget, pastReturn, phaseEffects, planForward, refusedWhileOwed, releasePlan, requestName, requestPreview, requireOwner, resolveVolume, seedLandingRefusal, shrinkWhileDeploying, storeRefusal, takeLane, unanswered, unreachable, withSlotMachine } from "#/modules/volume-run/volume-run.rows.server";

/**
 * A requested run planned against the copies the Servers hold now, so a refusal reaches the caller before any row
 * exists. The run plans again when it starts, since the copies may move in between.
 */
export const planVolumeRun = Effect.fn("VolumeRun.plan")(function* (
  organizationId: string,
  input: PlanRequest,
) {
  const { environment, born, ...volume } = yield* resolveVolume(organizationId, input.environment, input.volumeId);
  if (input.kind === "seed") {
    const refusal = yield* seedLandingRefusal(organizationId, input.environment, input.volumeId, input.args.into.volumeId);
    if (refusal !== null) return { ok: false, refusal } satisfies VolumeRunPlan;
  }
  const lane = yield* activeLane(input.volumeId);
  if (lane !== undefined && (input.kind !== "release" || keepsLane(lane, "cancelled", "release"))) {
    return { ok: false, refusal: yield* laneRefusal(lane, input.kind) } satisfies VolumeRunPlan;
  }
  if ((input.kind === "mirror" || input.kind === "move") && born) {
    const refusal = yield* originRefusal(organizationId, volume, [], unanswered, input.args);
    if (refusal !== null) return { ok: false, refusal } satisfies VolumeRunPlan;
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
    const effects = planned.deletes?.map((member) => deletesCopy(volume, member));
    return { ok: false, refusal: storeRefusal(planned.refusal), ...(effects !== undefined && { effects }) } satisfies ClusterPlan;
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
    ? yield* answered(keptOn(organizationId, volume.dockerVolume, holdingNewest(from.slots).map((member) => member.machine)).pipe(
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
        if (closed !== undefined) yield* enqueueEvent(volumeRunTaken.create({ runId: closed.id }));
      }));
    }
  }
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

/** The reasons SeedVolume and the copy verbs may refuse with. */
export type SwitchReason = CopyRefusal | SwitchError["reason"];

const SWITCH_REASONS = [
  "mismatch", "no_transition", "busy", "no_capacity", "behind", "written", "no_source",
  "precondition", "volume_switching", "no_writer", "restore_point_gone", "stale", "foreign", "docker_unavailable",
] as const satisfies readonly SwitchReason[];

/** Refusals that pass on their own, so they fail only the attempt and the step asks again. Docker comes back by itself. */
const passing = (reason: SwitchReason | null) => reason === "docker_unavailable";

const SwitchErrorDetails = Schema.Struct({ details: Schema.Struct({ reason: Schema.Literals(SWITCH_REASONS) }) });

const decodeSwitchError = Schema.decodeUnknownOption(SwitchErrorDetails);

export function switchErrorOf(error: PloyzSdkError): SwitchReason | null {
  const decoded = decodeSwitchError("cause" in error ? error.cause : undefined);
  return Option.isSome(decoded) ? decoded.value.details.reason : null;
}

/** `fenced` names the Volume a Stale or Foreign refusal is about: the run's own, except a Seed's `into`. */
export function switchFailureMessage(
  reason: SwitchReason,
  ctx: Pick<RunContext, "volumeName">,
  machine: MachineRef,
  fenced = ctx.volumeName,
): string | undefined {
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
      return `a newer run took ${fenced} on ${machine.name}`;
    case "foreign":
      return `${fenced} on ${machine.name} is mounted by a Service approved for other data`;
    case "docker_unavailable":
      return `Docker isn't answering on ${machine.name}`;
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

/**
 * A refusal with a message ends the run; Busy, Docker not answering and transport failures fail only the attempt, so
 * the step retries.
 * `fenced` names the Volume whose floor the request is checked against, as in `switchFailureMessage`.
 */
export const sendSwitch = <R extends SwitchRequest>(ctx: RunContext, inngestRunId: string, machine: MachineRef, request: R, fenced = ctx.volumeName) => {
  return Effect.gen(function* () {
    yield* requireOwner(ctx.id, inngestRunId);
    const session = yield* openSession(ctx.organizationId);
    return yield* session.volumeSwitch(machine.id, request).pipe(
      Effect.catch((error): Effect.Effect<never, Error, Database> => {
        const reason = switchErrorOf(error);
        const message = reason === null ? undefined : switchFailureMessage(reason, ctx, machine, fenced);
        if (message !== undefined && !passing(reason)) return failRun(ctx, inngestRunId, message);
        return Effect.fail(new SwitchAttemptError(reason, message ?? `${request.command} on ${machine.name}: ${sdkFailureMessage(error, [machine])}`));
      }),
    );
  }).pipe(Effect.scoped);
};

/** Plain JSON, since a step returns it: the copy the Machine holds now, after the step or after its refusal. */
export type CopyAnswer =
  | { readonly ok: true; readonly copy: VolumeCopy | null }
  | { readonly ok: false; readonly reason: CopyRefusal | "unanswered"; readonly copy: VolumeCopy | null; readonly message: string };

/**
 * Who a SetVolumeCopy is for: a run, a Deploy following another run's lane without owning its row, or the rowless
 * mirror sync at the Volume's newest gen.
 */
export type CopyAsker = Pick<RunContext, "organizationId" | "dockerVolume" | "volumeName" | "gen"> & Partial<Pick<RunContext, "id">>;

/**
 * One SetVolumeCopy. A refusal is an answer, since the caller decides what follows it; only a failure to ask, or a
 * Machine whose Docker isn't answering, fails, so a run's step retries. Spent Cluster time fails it before it opens a session, and out of time is `inFlight` only
 * when the SetVolumeCopy itself was cut. Any other failure but a `SwitchAttemptError` is a session that didn't open, so
 * the SetVolumeCopy was never sent.
 */
export const askCopy = Effect.fn("VolumeRun.askCopy")(function* (
  asker: CopyAsker,
  machine: MachineRef,
  expect: ExpectedCopy,
  want: WantedCopy,
) {
  yield* requireClusterTime;
  const session = yield* openSession(asker.organizationId).pipe(
    Effect.catchIf((error) => error instanceof ClusterOutOfTime, () => Effect.fail(new ClusterOutOfTime({ inFlight: false }))),
  );
  const payload = { name: asker.dockerVolume, expect, want, gen: asker.gen };
  return yield* session.volumeSwitch(machine.id, { command: "set_volume_copy", payload }).pipe(
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
      if (passing(reason)) return Effect.fail(new SwitchAttemptError(reason, message));
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

/**
 * The one Service container on `machine` that mounts this Volume, whose image B needs before the cut. A running one wins
 * over stopped ones: a Server that enforces device approval keeps the Container an earlier Move's cut stopped.
 */
export const findHolder = Effect.fn("VolumeRun.findHolder")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  const on = (yield* serviceContainers(ctx.organizationId, ctx.dockerVolume)).filter((container) => container.machine_id === machine.id);
  const running = on.filter((container) => container.runtime.state === "running");
  const holders = running.length > 0 ? running : on;
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

/**
 * With only slots left, the cut's source is the one of them that keeps the newest Service container for this Volume.
 * One Writer runs at a time and each Move creates its Container on the target, so the newest is the one the latest
 * cut stopped. A Server that enforces device approval keeps the Container each Move's cut stopped there, so older
 * ones may stay on any of them; the Server itself starts only a Container its open cut recorded. Two Servers whose
 * newest were made at once name no source. Creation times come from each Server's clock, so a release first narrows
 * the slots to those holding the newest snapshot (`holdingNewest`).
 */
export const keptOn = Effect.fn("VolumeRun.keptOn")(function* (organizationId: string, dockerVolume: string, machines: readonly MachineRef[]) {
  const on = (yield* serviceContainers(organizationId, dockerVolume)).filter((container) =>
    machines.some((candidate) => candidate.id === container.machine_id));
  const [newest, next] = newestFirst(on);
  if (next !== undefined && next.machine_id !== newest?.machine_id && next.created_at_unix_nanos === newest?.created_at_unix_nanos) return null;
  const machine = newest === undefined ? undefined : machines.find((candidate) => candidate.id === newest.machine_id);
  return newest === undefined || machine === undefined ? null : { machine, containerId: newest.container_id } satisfies Kept;
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
export const commitMove = Effect.fn("VolumeRun.commit")(function* (
  ctx: Pick<RunContext, "id">,
  inngestRunId: string,
  to: MachineId,
) {
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
 * when B is asked again and fails while its copy stays unrouted. With no stopped Container to start from, a B that
 * holds its own Service becomes the writer and stays stopped: an earlier start moved the Service there, or a Deploy's
 * placement removed the source's Container. `ownedBy` is the Inngest run that must still own the row when B is
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
  const [holder] = from === null ? stopped : newestFirst(stopped);
  if (holder === undefined && containers.some((container) => container.machine_id === to.id)) {
    if (ownedBy !== null) yield* requireOwner(ctx.id, ownedBy);
    return yield* askCopy(ctx, to, start.expect, { kind: "writer", at: start.at, epoch: start.epoch, start: null });
  }
  if (holder === undefined) {
    return yield* Effect.fail(new NonRetriableError(`${ctx.volumeName} has no stopped Service container${from === null ? "" : ` on ${from.name}`} to start on ${to.name}`));
  }
  if (from === null && stopped.length > 1) {
    const named = yield* Effect.forEach(stopped, (container) => machineNamed(ctx.organizationId, container.machine_id));
    const on = named.map((machine) => machine.name ?? machine.id).join(" and ");
    return yield* Effect.fail(new NonRetriableError(`${ctx.volumeName} has ${stopped.length} stopped Service containers on ${on}; starting it on ${to.name} needs exactly one`));
  }
  const session = yield* openSession(ctx.organizationId);
  const { container } = yield* session.inspectContainer(holder.machine_id, holder.container_id).pipe(
    Effect.mapError((error) => new Error(`reading ${holder.resolved_spec.name}'s spec: ${sdkFailureMessage(error)}`)),
  );
  if (ownedBy !== null) yield* requireOwner(ctx.id, ownedBy);
  return yield* askCopy(ctx, to, start.expect, {
    kind: "writer",
    at: start.at,
    epoch: start.epoch,
    start: {
      kind: "create",
      namespace: holder.namespace,
      resolved_spec: startingSpec(ctx, container.resolved_spec),
      creation_key: creationKey(ctx, start.at),
      gen: container.gen ?? 1,
      rotation: container.rotation === "in" ? { in: { run: null, expect: "pending" } } : "pending",
    },
  });
}, Effect.scoped);

/**
 * The stopped Container's spec as B starts it, from the image already copied there. The spec was resolved at the
 * Service's last Deploy; a resize since then raised the copies, and the Store recorded it.
 */
const startingSpec = (ctx: Pick<RunContext, "dockerVolume" | "recordedBytes">, resolved: ResolvedServiceSpec): ResolvedServiceSpec => {
  const volumes = resolved.volumes.map((volume) =>
    volume.source.kind === "provisioned" && volume.source.name === ctx.dockerVolume && ctx.recordedBytes > 0
      ? { ...volume, source: { ...volume.source, maximum_bytes: ctx.recordedBytes } }
      : volume);
  return { ...resolved, volumes, container: { ...resolved.container, pull_policy: "never" } };
};

/**
 * Removes the stopped Service containers the Move left on `machines`; a rerun finds the ones already gone gone. Returns
 * a sentence for each that stays.
 */
export const removeKept = Effect.fn("VolumeRun.removeKept")(function* (ctx: RunContext, inngestRunId: string, machines: readonly MachineRef[]) {
  yield* requireOwner(ctx.id, inngestRunId);
  const containers = yield* serviceContainers(ctx.organizationId, ctx.dockerVolume);
  const stays: string[] = [];
  for (const container of containers) {
    const machine = machines.find((candidate) => candidate.id === container.machine_id);
    if (machine === undefined || container.runtime.state === "running") continue;
    const stay = yield* removeContainer(ctx, inngestRunId, machine, container.container_id);
    if (stay !== null) stays.push(stay);
  }
  return stays;
});

/** The remove's answer when Docker has the Container and its Server holds no Resolved Service Spec under its ID. */
const KeptWithoutSpec = Schema.Struct({
  code: Schema.Literal("conflict"),
  details: Schema.Struct({ reason: Schema.Literal("no_resolved_spec") }),
});

const keptWithoutSpec = (error: PloyzSdkError) =>
  "cause" in error && Option.isSome(Schema.decodeUnknownOption(KeptWithoutSpec)(error.cause));

/**
 * Removes one Container, or returns the sentence saying it stays, when its Server holds no spec for it.
 */
export const removeContainer = Effect.fn("VolumeRun.removeContainer")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef, containerId: ContainerId) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  return yield* session.removeContainer(machine.id, containerId).pipe(
    Effect.as(null),
    Effect.catchIf(keptWithoutSpec, () => Effect.succeed(`${containerId} stays on ${machine.name}, not removed`)),
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
          checksOrigins: checksOrigins(capabilities),
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

/** What a Volume's placement and mirror sync ask the Servers with. The real asks, unless a test provides others. */
export const VolumeCluster = Context.Reference<{
  readonly inspect: typeof inspectVolumeCopies;
  readonly observe: typeof observeVolumeMembers;
  readonly ask: typeof askCopy;
}>("ployz/VolumeCluster", {
  defaultValue: () => ({ inspect: inspectVolumeCopies, observe: observeVolumeMembers, ask: askCopy }),
});
