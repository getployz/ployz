import "@tanstack/react-start/server-only";
import type { ContainerId, ContainerObservation, CopyRefusal, CopyStart, DockerVolume, EnvironmentRef, ExpectedCopy, MachineId, MachineName, ResolvedServiceSpec, SnapshotGuid, SwitchError, VolumeCopy, VolumeSwitchRequest, WantedCopy } from "@ployz/sdk";
import { and, eq, inArray, notInArray, or, sql } from "drizzle-orm";
import { Context, Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { OperationAsked, OperationVerb } from "#/modules/operations/asked";
import { volumeRunTaken } from "#/modules/volume-run/volume-run.events";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { ClusterOutOfTime, clusterRead, PLAN_CLUSTER_BUDGET_MS, requireClusterTime, RUNTIME_FRAME_TIMEOUT_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { type Requirement, requirementFor, requirements, serverSupports, writerStart } from "#/modules/runtime/daemon-compat";
import { type PloyzSdkError, RpcError, sdkFailureMessage, setVolumeCopyRefusal } from "#/modules/runtime/ployz.server";
import { alreadyOn, confirmedCopy, destroyedBy, holdingNewest, type PlanInput, planForwardRelease, planFromCopies, restoreSource, roleOf, upgradeBeforeSend, writerRemovedMessage } from "#/modules/volume-run/plan";
import { resizeRequirements, sizeDirection } from "#/modules/volume-run/resize";
import { isBehind, isCurrent, releaseLine } from "#/modules/server-upgrade/server-upgrade";
import { channelRelease, organizationReleaseChannel } from "#/modules/server-upgrade/server-upgrade.server";
import { volumeRun } from "#/modules/volume-run/tables";
import { checksOrigins, keepsWritten } from "#/modules/runtime/daemon-release";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, isDestination, type MachineRef, type Member, needsFloor, RUN_FEATURES, type ServiceSpec, type VolumeLaneInput } from "#/modules/volume-run/volume-run";
import { managementAddress } from "#/modules/runtime/naming";
import { type VolumeRunKind } from "#/modules/volume-run/volume-run-states";
import { Database } from "#/server/database.server";
import { listNames } from "#/lib/plural";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { REGION_LABEL } from "#/modules/runtime/runtime-watch-frame";
import { type ClusterPlan, type LaneTaker, OBSERVE_TIMEOUT, type Owed, PROTECTED_VERBS, type PlanRequest, type ResolvedVolume, type RunContext, VERBS, type VolumeRunPlan, activeLane, answered, commitment, committedTo, deletesCopy, deploymentsSeen, ended, failRun, keepsLane, laneInputOf, laneRefusal, machineNamed, namedOwedRefusal, newestFirst, openSession, originRefusal, owedTarget, pastReturn, phaseEffects, planForward, readRow, recordRemoved, refusedWhileOwed, releasePlan, requestName, requestPreview, requireOwner, resolveVolume, seedLandingRefusal, shrinkWhileDeploying, storeRefusal, takeLane, unanswered, unreachable, withSlotMachine } from "#/modules/volume-run/volume-run.rows.server";

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
  // A delete's approval names each copy as the Servers report it now, so a copy replaced or written since asks again.
  const copies = phase.kind === "delete_mirror" ? phase.destroy.flatMap((member) => confirmedCopy(member) ?? []) : undefined;
  const askedAs = (verb: OperationVerb): OperationAsked => ({
    subject: `volume:${input.volumeId}`,
    verb,
    name: requestName(volume, input),
    preview: { volume: input.volumeId, copies: effects.map((effect) => effect.node), ...requestPreview(volume, input, members.value), ...(copies !== undefined && { confirms: copies }) },
    effects,
  });
  const asked = verb === null ? null : askedAs(verb);
  const guarding = PROTECTED_VERBS[input.kind];
  const guard = asked ?? (guarding === null ? null : askedAs(guarding));
  const ids = (members: readonly AnsweredMember[]) => members.map((member) => member.machine.id);
  // A move records its Server's id, so what guards that Server finds it by id whatever the Server is called later.
  const pinned = input.kind === "release"
    ? { ...input, args: { plan: { sources: ids(release.sources), forgets: ids(release.forgets) } } }
    : resolved.kind === "move" && (phase.kind === "move" || phase.kind === "landed")
      ? { ...resolved, args: { ...resolved.args, machine: phase.target.machine.id } }
      : resolved;
  return { ok: true, volume, asked, guard, input: pinned, ...(copies !== undefined && { copies }) } satisfies ClusterPlan;
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
  "precondition", "volume_switching", "no_writer", "restore_point_gone", "stale", "foreign", "docker_unavailable", "held", "stopped",
  "written_here",
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
      return `a later run reached ${fenced} on ${machine.name} first`;
    case "foreign":
      return `${fenced} on ${machine.name} is mounted by a Service approved for other data`;
    case "docker_unavailable":
      return `Docker isn't answering on ${machine.name}`;
    case "held":
      return heldMessage(name, machine, null);
    case "stopped":
      return `${name}'s Service no longer runs on ${machine.name}, so the move stopped nothing`;
    case "written_here":
      return `${name}'s copy on ${machine.name} may hold writes no other Server has; only ployz volume mirror rm deletes it`;
    case "busy":
      return undefined;
  }
}

const SaidByServer = Schema.Struct({ message: Schema.String });

/**
 * A refusal from a Ployz newer than this Cloud, whose reason it can't read: the Server's own words, since only they know
 * what it needs.
 */
function unrecognizedMessage(error: PloyzSdkError, machine: MachineRef) {
  const said = Schema.decodeUnknownOption(SaidByServer)("cause" in error ? error.cause : undefined);
  return Option.isSome(said) && said.value.message.length > 0
    ? `${machine.name} refused: ${said.value.message}`
    : `${machine.name} refused for a reason this Ployz Cloud doesn't know; upgrade Ployz Cloud`;
}

/**
 * The failure a Seed's reply ends its run on, or `null` while the copy runs or once it landed. A copy that failed while
 * part of it still runs shows as receiving with its failure: no Seed answers that failure until the part exits.
 */
export function seedFailureMessage(copy: VolumeCopy | null, into: string, name: string, machine: MachineRef): string | null {
  if (copy?.kind !== "slot") return null;
  if (copy.pull?.failure) return `${into} could not copy ${name}: ${copy.pull.failure}`;
  const state = copy.pull_state;
  if (state?.kind === "receiving" && state.failure) {
    return `${into} could not copy ${name}: ${state.failure}. Part of the copy still runs on ${machine.name}; restart the Ployz daemon there or wait for it to exit`;
  }
  return null;
}

/** A cut refused before it stopped anything, since `holder` runs on the Volume beside the Service the run counted. */
const heldMessage = (name: string, machine: MachineRef, holder: string | null) =>
  `${holder ?? "another Container"} also uses ${name} on ${machine.name}. A move takes one Service, so stop it first`;

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

/**
 * Plain JSON, since a step returns it: the copy the Machine holds now, after the step or after its refusal. A step
 * refused `server_lacks` was never sent, so its copy is unknown.
 */
export type CopyAnswer =
  | { readonly ok: true; readonly copy: VolumeCopy | null }
  | { readonly ok: false; readonly reason: CopyRefusal | "unanswered" | "refused" | "server_lacks"; readonly copy: VolumeCopy | null; readonly message: string };

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
 *
 * Every SetVolumeCopy goes through here, so this is where a step that deletes or may replace a copy is checked: it is
 * sent only to a Server whose capabilities, read over the same session just before, keep a copy that may hold writes no
 * other Server has. Otherwise it is refused `server_lacks` and never sent. A pull names `adds`, the Writer's snapshot it
 * adds to, to say it can't replace the slot.
 */
export const askCopy = Effect.fn("VolumeRun.askCopy")(function* (
  asker: CopyAsker,
  machine: MachineRef,
  expect: ExpectedCopy,
  want: WantedCopy,
  adds?: SnapshotGuid,
) {
  yield* requireClusterTime;
  const session = yield* openSession(asker.organizationId).pipe(
    Effect.catchIf((error) => error instanceof ClusterOutOfTime, () => Effect.fail(new ClusterOutOfTime({ inFlight: false }))),
  );
  const destroys = destroyedBy(expect, want, adds);
  if (destroys !== null) {
    const frame = yield* session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(
      clusterRead,
      Effect.mapError((error) => error instanceof ClusterOutOfTime
        ? new ClusterOutOfTime({ inFlight: false })
        : new SwitchAttemptError(null, `reading what ${machine.name}'s Ployz keeps: ${error.message}`, "unreached")),
    );
    const listed = frame.machines.find((observed) => observed.machine.id === machine.id);
    if (listed === undefined) {
      return yield* Effect.fail(new SwitchAttemptError(null, `${machine.name} is missing from the Cluster's report, so it wasn't asked`, "unreached"));
    }
    if (!keepsWritten({ capabilities: listed.machine.runtime.capabilities ?? [] })) {
      return { ok: false, reason: "server_lacks", copy: null, message: upgradeBeforeSend(asker.volumeName, machine.name, destroys) } satisfies CopyAnswer;
    }
  }
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
      const message = reason === "held"
        ? heldMessage(asker.volumeName, machine, refused.holder ?? null)
        : reason === "unrecognized"
          ? unrecognizedMessage(error, machine)
          : switchFailureMessage(reason, asker, machine) ?? `${machine.name} is busy with ${asker.volumeName}`;
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
  adds?: SnapshotGuid,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* askCopy(ctx, machine, expect, want, adds);
});

export const inspectCopy = (ctx: RunContext, inngestRunId: string, machine: MachineRef) =>
  sendSwitch(ctx, inngestRunId, machine, { command: "inspect_volume_copy", payload: { name: ctx.dockerVolume } }).pipe(
    Effect.map((view) => view.copy),
  );

export type Holder = ServiceSpec & { readonly containerId: ContainerId; readonly running?: boolean };

/** Drain::DEFAULT: how long the cut lets requests in flight finish once the Container is out of ingress. */
export const DRAIN_MS = 3_000;

/** The Containers that mount this Volume, and each Server's region label, from one frame. */
const volumeFrame = Effect.fn("VolumeRun.volumeFrame")(function* (organizationId: string, dockerVolume: string) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead);
  const containers = frame.containers.filter((container) =>
    container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === dockerVolume));
  const regionOf = (machine: MachineId) => frame.machines.find((observed) => observed.machine.id === machine)?.machine.labels[REGION_LABEL] ?? null;
  const runtimeOf = (machine: MachineId): ServerRuntime => {
    const observed = frame.machines.find((candidate) => candidate.machine.id === machine)?.machine.runtime;
    return { capabilities: observed?.capabilities ?? [], daemonVersion: observed?.daemon_version ?? "" };
  };
  return { containers, regionOf, runtimeOf };
}, Effect.scoped);

const serviceContainers = (organizationId: string, dockerVolume: string) =>
  volumeFrame(organizationId, dockerVolume).pipe(Effect.map(({ containers }) => containers.filter((container) => container.kind === "service_container")));

type Refused = { readonly ok: false; readonly refusal: { readonly code: "invalid"; readonly message: string } };

const refused = (message: string): Refused => ({ ok: false, refusal: { code: "invalid", message } });

/**
 * The one Container on `machine` that mounts this Volume, whose Service B starts: `chosen`, the one the run picked
 * before its cut, else a running one over stopped ones, since a Server that enforces device approval keeps the
 * Container an earlier Move's cut stopped. It must be a Service's, the only one running on the Volume, and mount no
 * other data, since only this Volume moves with it. It must not be a Global's copy: a copy stays routed while it
 * crash-loops, so the one the cut stops could be the last that works.
 */
const holderOn = (
  ctx: Pick<RunContext, "volumeName" | "dockerVolume">,
  containers: readonly ContainerObservation[],
  machine: MachineRef,
  chosen: ContainerId | null = null,
) => {
  const on = containers.filter((container) => container.machine_id === machine.id);
  const running = on.filter((container) => container.runtime.state === "running");
  // Once the cut stopped it, the newest stopped one is the Service's: older ones are what earlier Moves kept.
  const [holder] = chosen !== null
    ? on.filter((container) => container.container_id === chosen)
    : running.length > 0 ? running : newestFirst(on.filter((container) => container.kind === "service_container"));
  if (holder === undefined) return refused(`${ctx.volumeName} has no Service on ${machine.name} to move`);
  if (running.length > 1) {
    const named = listNames(running.map((container) => container.display_name));
    return refused(`${named} all use ${ctx.volumeName} on ${machine.name}. A move takes one Service, so stop the others first`);
  }
  if (holder.kind !== "service_container") {
    return refused(`${holder.display_name} uses ${ctx.volumeName} on ${machine.name}, and a move takes only a Service. Stop it first`);
  }
  if (holder.resolved_spec.mode.mode === "global") {
    return refused(`${holder.resolved_spec.name} runs on every server, and a move takes a Service with a replica count`);
  }
  const others = holder.resolved_spec.volumes.filter(({ source }) =>
    source.kind !== "tmpfs" && !(source.kind === "provisioned" && source.name === ctx.dockerVolume));
  if (others.length > 0) {
    const named = listNames(others.map((volume) => volume.reference));
    return refused(`${holder.resolved_spec.name} also mounts ${named}, which can't move with ${ctx.volumeName}`);
  }
  return { ok: true, holder } as const;
};

/** The one Service container on `machine` that mounts this Volume, read without a run: a drain asks before it admits one. */
export const readHolder = Effect.fn("VolumeRun.readHolder")(function* (
  ctx: Pick<RunContext, "organizationId" | "dockerVolume" | "volumeName">,
  machine: MachineRef,
) {
  const found = holderOn(ctx, (yield* volumeFrame(ctx.organizationId, ctx.dockerVolume)).containers, machine);
  if (!found.ok) return found;
  const { holder } = found;
  const holderOf: Holder = {
    containerId: holder.container_id,
    namespace: holder.namespace,
    redactedSpec: holder.resolved_spec,
    running: holder.runtime.state === "running",
  };
  return { ok: true, holder: holderOf } as const;
});

/** The one Service container on `machine` that mounts this Volume, whose image B needs before the cut. */
export const findHolder = Effect.fn("VolumeRun.findHolder")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* readHolder(ctx, machine);
});

type CreateStart = Extract<CopyStart, { kind: "create" }>;

/** What a Server's daemon lists and runs, as the runtime frame reports it: none for a Server it doesn't list. */
type ServerRuntime = { readonly capabilities: readonly string[]; readonly daemonVersion: string };

/**
 * The Create that starts `holder`'s Service on `to`: its spec, in `to`'s region when the Service is kept in one, with the
 * signed versions or rule build that admit it there. Read from the Store each time, so a step result never holds the spec.
 * `moved` once the Volume's writer is `to`, when a refusal asks for a Deploy and no other move.
 */
const createOn = Effect.fn("VolumeRun.createOn")(function* (
  ctx: Pick<RunContext, "organizationId" | "volumeId" | "volumeName" | "dockerVolume" | "recordedBytes">,
  holder: ContainerObservation,
  to: MachineRef,
  region: string | null,
  runtime: ServerRuntime,
  at: SnapshotGuid,
  moved: boolean,
  copied: boolean,
) {
  // Every Writer Create comes through here, a Move's check and start and a forward Release's start alike, so this is
  // the one gate: a Server that lacks a verb would run the start differently, or not at all.
  const unstartable = writerStart.find((requirement) => serverSupports(runtime, requirement) !== true);
  if (unstartable !== undefined) {
    const { lack } = yield* lackSentence(ctx.organizationId, unstartable.capability, { name: to.name, daemonVersion: runtime.daemonVersion }, undefined);
    return refused(lack);
  }
  const session = yield* openSession(ctx.organizationId);
  const { container } = yield* session.inspectContainer(holder.machine_id, holder.container_id).pipe(
    Effect.mapError((error) => new Error(`reading ${holder.resolved_spec.name}'s spec: ${sdkFailureMessage(error)}`)),
  );
  const store = yield* cloudStore;
  const query = { namespace: holder.namespace, spec: container.resolved_spec, machine: to.id, ...(region !== null && { region }) };
  const proof = yield* storeTry(() => store.volumeStartProof(ctx.organizationId, query)).pipe(
    Effect.mapError((error) => new Error(`looking up ${holder.resolved_spec.name}'s approval: ${error.message}`)),
  );
  if (proof.kind !== "admits") {
    const service = holder.resolved_spec.name;
    const then = moved ? ` to start it on ${to.name}` : `, then move ${ctx.volumeName}`;
    return refused(proof.kind === "off_rule"
      ? `${to.name} can't start ${service}: the deploy rule that built it doesn't list ${to.name}. Add ${to.name} to the rule or deploy ${service} again${then}`
      : `${to.name} can't start ${service}: no approved version of ${service} runs there. Deploy ${service}${moved ? then : ` so it can${then}`}`);
  }
  const start: CreateStart = {
    kind: "create",
    namespace: holder.namespace,
    resolved_spec: startingSpec(ctx, proof.spec, copied),
    creation_key: creationKey(ctx, at),
    gen: container.gen ?? 1,
    rotation: container.rotation === "in" ? { in: { run: null, expect: "pending" } } : "pending",
    versions: proof.versions,
    ...(proof.rule !== undefined && { rule: proof.rule }),
    // A registry rule's Server pulls the image itself, with the Service's login.
    ...(proof.registry_login !== undefined && { registry_login: proof.registry_login }),
  };
  return { ok: true, start } as const;
}, Effect.scoped);

/**
 * Why `to` can't take the Service `from` runs on this Volume, or null when it can. Asked before the run stops anything
 * and again just before the commit, on `chosen` once the run picked it: `from` runs one Service on the Volume and
 * nothing else, an approval admits its spec on `to`, and `to` checks that start as its Create would, changing nothing.
 */
export const startRefusal = Effect.fn("VolumeRun.startRefusal")(function* (
  ctx: CopyAsker & Pick<RunContext, "volumeId" | "recordedBytes">,
  from: MachineRef,
  to: MachineRef,
  expect: ExpectedCopy,
  chosen: ContainerId | null = null,
) {
  const { containers, regionOf, runtimeOf } = yield* volumeFrame(ctx.organizationId, ctx.dockerVolume);
  const found = holderOn(ctx, containers, from, chosen);
  if (!found.ok) return found.refusal.message;
  // A check changes nothing, so the snapshot and epoch it names are never used; a Server still parses the snapshot's number.
  const created = yield* createOn(ctx, found.holder, to, regionOf(to.id), runtimeOf(to.id), "check", false, true);
  if (!created.ok) return created.refusal.message;
  const checked = yield* askCopy(ctx, to, expect, { kind: "writer", at: "0", epoch: 0, start: { ...created.start, check: true } }).pipe(
    Effect.catchIf((error) => error instanceof SwitchAttemptError && error.reach === "answered" && error.reason === null, (error) => Effect.succeed<CopyAnswer>({
      ok: false, reason: "refused", copy: null, message: error.message,
    })),
  );
  return checked.ok ? null : checked.message;
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
 * The commit: this run makes `to` the writer at snapshot `at` and `epoch`, which every later finish of it asks for. It
 * wins once, and again for the run that already won it or a release that inherited it; a row taken over or committed
 * elsewhere loses.
 */
export const commitMove = Effect.fn("VolumeRun.commit")(function* (
  ctx: Pick<RunContext, "id">,
  inngestRunId: string,
  to: MachineId,
  at: SnapshotGuid,
  epoch: number,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.update(volumeRun)
    .set({ phase: "committed", current: to, at, epoch, updatedAt: sql`now()` })
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
 * one stopped Container on any other Machine. `holder` is the Container the run chose on `from` before its cut, if any.
 */
type WriterStart = {
  readonly to: MachineRef;
  readonly from: MachineRef | null;
  readonly holder?: ContainerId | null;
  readonly expect: ExpectedCopy;
  readonly at: SnapshotGuid;
  readonly epoch: number;
};

/**
 * The point of no return: the commit, then B becomes the writer at G and creates the Service from A's stopped
 * Container. One step, so B is never told to start by a run that does not own the commit, and the spec never
 * lands in a step result. A B that already runs the Service is left alone. A Move asks B once more just before it
 * commits, since an approval may have changed since the rounds; a refusal there leaves the Move uncommitted, so its
 * release starts the Service on A again. A replay reads whether the commit landed from the row.
 */
export const commitAndStart = Effect.fn("VolumeRun.commitAndStart")(function* (
  ctx: RunContext,
  inngestRunId: string,
  start: WriterStart,
) {
  const { to, from } = start;
  const row = yield* readRow(ctx.id);
  if (from !== null && row?.phase !== "committed") {
    const refusal = yield* startRefusal(ctx, from, to, start.expect, start.holder ?? null);
    if (refusal !== null) return { ok: false, reason: "refused", copy: null, message: refusal } satisfies CopyAnswer;
  }
  if (!(yield* commitMove(ctx, inngestRunId, to.id, start.at, start.epoch))) {
    const lost: CopyAnswer = { ok: false, reason: "stale", copy: null, message: `a newer run took ${ctx.volumeName} before ${to.name} started it` };
    return lost;
  }
  return yield* startWriter(ctx, inngestRunId, start);
});

/**
 * B becomes the writer at G and creates the Service from the source's stopped Container, at its gen and routed once
 * healthy when the source was routed, carrying the approval that admits it on B; a B already running it is left alone,
 * unless the source is routed and B is not, when B is asked again and fails while its copy stays unrouted. With no
 * stopped Container to start from, a B that holds its own Service becomes the writer and stays stopped: an earlier
 * start moved the Service there, or a Deploy's placement removed the source's Container.
 */
export const startWriter = Effect.fn("VolumeRun.startWriter")(function* (ctx: RunContext, ownedBy: string, start: WriterStart) {
  const { to, from } = start;
  const { containers: all, regionOf, runtimeOf } = yield* volumeFrame(ctx.organizationId, ctx.dockerVolume);
  const containers = all.filter((container) => container.kind === "service_container");
  const stopped = containers.filter((container) =>
    container.runtime.state !== "running" && (from === null ? container.machine_id !== to.id : container.machine_id === from.id));
  const routed = stopped.some((container) => container.rotation === "in");
  if (containers.some((container) =>
    container.machine_id === to.id && container.runtime.state === "running" && (container.rotation === "in" || !routed))) {
    // The frame may be stale, so B itself says it writes at this commitment's epoch, which it took at the committed snapshot.
    const copy = yield* inspectCopy(ctx, ownedBy, to);
    if (copy?.kind === "root" && !copy.readonly && copy.epoch === start.epoch) {
      const running: CopyAnswer = { ok: true, copy };
      return running;
    }
    const message = `${to.name} runs ${ctx.volumeName}'s Service but doesn't write ${ctx.volumeName} at this Move's snapshot`;
    return { ok: false, reason: "refused", copy, message } satisfies CopyAnswer;
  }
  const chosen = start.holder ?? null;
  const [holder] = chosen !== null ? stopped.filter((container) => container.container_id === chosen) : from === null ? stopped : newestFirst(stopped);
  if (holder === undefined && containers.some((container) => container.machine_id === to.id)) {
    yield* requireOwner(ctx.id, ownedBy);
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
  // A Move copied the image from `from` before its start; a forward Release, with no `from`, copied none.
  const created = yield* createOn(ctx, holder, to, regionOf(to.id), runtimeOf(to.id), start.at, true, from !== null);
  if (!created.ok) return yield* Effect.fail(new NonRetriableError(created.refusal.message));
  yield* requireOwner(ctx.id, ownedBy);
  return yield* askCopy(ctx, to, start.expect, { kind: "writer", at: start.at, epoch: start.epoch, start: created.start });
});

/**
 * The stopped Container's spec as B starts it. B pulls nothing when the run `copied` the image there; otherwise it
 * pulls the image when it isn't there, as every Service's spec asks, so a forward Release starts once the registry
 * answers. The policy is set either way: a holder an earlier Move started carries that Move's `never`. The spec was
 * resolved at the Service's last Deploy; a resize since then raised the copies, and the Store recorded it.
 */
const startingSpec = (ctx: Pick<RunContext, "dockerVolume" | "recordedBytes">, resolved: ResolvedServiceSpec, copied: boolean): ResolvedServiceSpec => {
  const volumes = resolved.volumes.map((volume) =>
    volume.source.kind === "provisioned" && volume.source.name === ctx.dockerVolume && ctx.recordedBytes > 0
      ? { ...volume, source: { ...volume.source, maximum_bytes: ctx.recordedBytes } }
      : volume);
  return { ...resolved, volumes, container: { ...resolved.container, pull_policy: copied ? "never" : "missing" } };
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
 * Removes one Container, recording on the run that its Server acknowledged the removal, or returns the sentence saying
 * it stays, when its Server holds no spec for it.
 */
export const removeContainer = Effect.fn("VolumeRun.removeContainer")(function* (ctx: RunContext, inngestRunId: string, machine: MachineRef, containerId: ContainerId) {
  yield* requireOwner(ctx.id, inngestRunId);
  const session = yield* openSession(ctx.organizationId);
  const stay = yield* session.removeContainer(machine.id, containerId).pipe(
    Effect.as(null),
    Effect.catchIf(keptWithoutSpec, () => Effect.succeed(`${containerId} stays on ${machine.name}, not removed`)),
    Effect.mapError((error) => new Error(`removing ${ctx.volumeName}'s old Container on ${machine.name}: ${sdkFailureMessage(error, [machine])}`)),
  );
  if (stay === null) yield* recordRemoved(ctx, inngestRunId, containerId);
  return stay;
}, Effect.scoped);

/** A run as the observe step reads it: a resize carries the size the Store records, which decides its direction. */
export type ObservedRun = Exclude<VolumeLaneInput, { kind: "resize" }> | (Extract<VolumeLaneInput, { kind: "resize" }> & { readonly recordedBytes: number });

const storageOf = (volumes: readonly DockerVolume[], machineId: MachineId, dockerVolume: string) =>
  volumes.find((volume) => volume.id.machine_id === machineId && volume.id.name === dockerVolume)?.storage;

const boundOf = (volumes: readonly DockerVolume[], machineId: MachineId, dockerVolume: string) => {
  const storage = storageOf(volumes, machineId, dockerVolume);
  return storage?.kind === "provisioned" ? storage.bound_bytes : null;
};

/** A copy that isn't a slot is a root, writable or read-only in a cut; an unset role is a writer. */
const lastRoot = (volumes: readonly DockerVolume[], machineId: MachineId, dockerVolume: string) => {
  const storage = storageOf(volumes, machineId, dockerVolume);
  return storage?.kind === "provisioned" && storage.role !== "slot";
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
    // `createOn` gates every Writer Create. A Move's destination is refused here too, so the Move is refused before
    // any row rather than once its check would be sent.
    const unstartable = run?.kind === "move" && isDestination(run, ref)
      ? writerStart.find((requirement) => serverSupports(capabilities, requirement) !== true)
      : undefined;
    if (unstartable !== undefined) return lacks(unstartable.capability, "move");
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
          keepsWritten: keepsWritten(capabilities),
          ...sized,
        })),
      Effect.catchTag("ServerLacks", (lack) => lacks(lack.capability, run?.kind)),
      // Any other failure to ask reads as silence, so nothing plans from a copy it never heard about.
      Effect.orElseSucceed((): Member => ({ machine: ref, answered: false })),
    );
  }, { concurrency: "unbounded" });
  return [...listed, ...stateless, ...unlisted].map((member): Member =>
    !member.answered && lastRoot(frame.volumes, member.machine.id, dockerVolume) ? { ...member, lastRoot: true } : member);
}, Effect.scoped);

const lackSentence = Effect.fn("VolumeRun.lackSentence")(function* (
  organizationId: string, capability: string, server: { readonly name: string; readonly daemonVersion: string }, kind: VolumeRunKind | undefined,
) {
  const required = requirementFor(capability) ?? requirements.volumeMove;
  const feature: Requirement = kind === undefined ? required : { capability: required.capability, feature: RUN_FEATURES[kind] };
  const line = releaseLine(server.daemonVersion);
  const release = line === null ? null : yield* organizationReleaseChannel(organizationId).pipe(
    Effect.flatMap((channel) => channelRelease(channel, line).pipe(Effect.timeout(OBSERVE_TIMEOUT))),
    // An unread release channel only drops the upgrade hint; the sentence still says what the Server lacks.
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
