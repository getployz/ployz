import type { ContainerId, ExpectedCopy, MachineId, Snapshot, VolumeCopy, WantedCopy } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import { inngestRunStatus, type PloyzInngest, type PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestFunctionFinishedEnvelopeSchema,
  inngestFunctionFinishedEventType,
  volumeRunRequestedEventType,
  volumeRunTakenEventType,
} from "#/modules/inngest/events";
import { PATIENCE, PATIENCE_WAITS } from "#/modules/volume-run/patience";
import { fenceTaken } from "#/modules/volume-run/volume-run.deploy.server";
import { alreadyOn, epochOf, expectOf, observedEpoch, type PlanInput, type Planned, planFromCopies } from "#/modules/volume-run/plan";
import { type AnsweredMember, copyName, type Member, type VolumeRunArgs } from "#/modules/volume-run/volume-run";
import {
  claimVolumeRun,
  cleanOrphans,
  closeStaleVolumeRuns,
  closeVolumeRun,
  commitAndStart,
  type CopyAnswer,
  copyImage,
  DRAIN_MS,
  endRun,
  failAndRelease,
  failRun,
  findHolder,
  findKept,
  inspectCopy,
  laneTarget,
  type MachineRef,
  observeMembers,
  organizationsWithMachines,
  refuseRun,
  removeContainer,
  removeKept,
  type RunContext,
  sendSwitch,
  setVolumeCopy,
  SwitchAttemptError,
  takeEpoch,
} from "#/modules/volume-run/volume-run.server";
import { runInngestEffect } from "#/server/run.server";
import type { AppServices } from "#/server/runtime.server";

type StepTools = Pick<PloyzStepTools, "run" | "sleep" | "sleepUntil">;
type EffectRunner = typeof runInngestEffect;

export const RUN_VOLUME_FUNCTION_ID = "run-volume";
// A Volume written faster than it ships never converges, so the rounds are capped.
export const MAX_ROUNDS = 3;
// Docker decides how long a Container step takes; 120 checks five seconds apart wait out ten minutes of it.
const BUSY_WAIT = "5s";
const BUSY_CHECKS = 120;

const VolumeRunRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  volumeId: Schema.String.check(Schema.isNonEmpty()),
  runId: Schema.String.check(Schema.isUUID()),
  took: Schema.optional(Schema.String),
});

type PhaseOf<K> = Extract<Extract<Planned, { ok: true }>["phase"], { kind: K }>;
type RunSteps = {
  readonly step: StepTools;
  readonly runEffect: EffectRunner;
  readonly run: RunContext;
  readonly runId: string;
};

export async function runVolume(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(VolumeRunRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const claim = await step.run("00-claim", () => runEffect(claimVolumeRun(request, runId)));
  if (claim.kind === "settled") return { runId: request.runId, settled: claim.state };
  // SAFETY: step output is the JSON of a RunContext, and a RunContext holds only JSON values.
  const run = claim.run as RunContext;

  if (request.took !== undefined) await step.run("01-fence", () => runEffect(fenceTaken(run, runId)));
  // SAFETY: step output is the JSON of the Member list, and a Member holds only JSON values.
  const observed = (await step.run("01-observe", () => runEffect(observeMembers(run, runId)))) as Member[];
  const steps: RunSteps = { step, runEffect, run, runId };
  const forward = run.kind === "release" && run.phase === "committed" && run.current !== null;
  const members = run.kind === "release" && !forward ? await floor(steps, observed) : observed;
  // SAFETY: the row's kind and args were written together from one VolumeRunInput.
  const input = { kind: run.kind, args: run.args, volumeName: run.volumeName, orphan: run.orphan } as PlanInput;
  const plan = planFromCopies(input, members);
  if (!plan.ok) {
    await step.run("02-refuse", () => runEffect(refuseRun(run, runId, plan.refusal)));
    return { runId: run.id, refused: plan.refusal };
  }
  if (forward && run.current !== null) return finishRelease(steps, members, run.current);
  const { phase } = plan;
  if (phase.kind === "seed") return seedVolume(steps, phase);
  if (phase.kind === "delete_mirror") return deleteMirror(steps, phase);
  if (phase.kind === "landed") return landVolume(steps, phase);
  // A plain sync row only demotes a primary that came back; with one primary, the rowless mirror sync refreshes the mirror.
  if (phase.kind === "sync" && !phase.full) {
    await finish(steps, "02-finish", `${run.volumeName} has one primary to keep`);
    return { runId: run.id, rounds: 0 };
  }
  const { epoch } = await step.run("02-epoch", () => runEffect(takeEpoch(run, runId, observedEpoch(members))));
  switch (phase.kind) {
    case "restore":
      return restoreVolume(steps, phase, epoch);
    case "demote":
      return demoteVolume(steps, phase, epoch);
    case "release":
      return releaseVolume(steps, phase, epoch);
    case "move":
      return moveVolume(steps, phase, epoch);
    case "mirror":
    case "sync":
      return mirrorVolume(steps, phase, epoch);
  }
}

/**
 * A release raises every copy's floor to its own gen before it plans, so the run it took over can switch nothing more.
 * Each reply is the copy as it settled: a cut caught before its flip is still the root, one past it is the slot.
 */
async function floor({ step, runEffect, run, runId }: RunSteps, members: readonly Member[]): Promise<Member[]> {
  const floored: Member[] = [];
  for (const member of members) {
    if (!member.answered) {
      floored.push(member);
      continue;
    }
    // A Machine with no copy is floored too, so the taken run's late Mirror cannot make one there.
    const answer = await step.run(`01-floor-${member.machine.name}`, () => runEffect(setVolumeCopy(run, runId, member.machine, expectOf(member.view.copy), { kind: "floor" })));
    floored.push({ ...member, view: { ...member.view, copy: answer.copy } });
  }
  return floored;
}

const fail = ({ step, runEffect, run, runId }: RunSteps, id: string, message: string): Promise<never> =>
  step.run(id, () => runEffect(failRun(run, runId, message))).then(() => {
    throw new NonRetriableError(message);
  });

/** A newer run fenced this one: it already ended its row if it still owned it, so it touches nothing more. */
class Stale extends NonRetriableError {}

/** A failure past the rounds ends the run and releases what it left in flight; a Stale run has nothing left to release. */
const release = ({ step, runEffect, run, runId }: RunSteps, id: string, error: Error): Promise<never> => {
  if (error instanceof Stale) return Promise.reject(error);
  return step.run(id, () => runEffect(failAndRelease(run, runId, error.message))).then(() => {
    throw new NonRetriableError(error.message);
  });
};

const stopped = (steps: RunSteps, id: string, error: Error): Promise<never> =>
  error instanceof Stale ? Promise.reject(error) : fail(steps, id, error.message);

const finish = ({ step, runEffect, run, runId }: RunSteps, id: string, message: string | null = null) =>
  step.run(id, () => runEffect(endRun(run, runId, "done", message)));

/**
 * One SetVolumeCopy. A Machine answers Busy while a Container step or another switch holds the Volume, so the run
 * sleeps and asks again with the same request, which spends no retry.
 */
async function settle({ step, runEffect, run, runId }: RunSteps, id: string, ask: Effect.Effect<CopyAnswer, Error, AppServices>, still: string) {
  for (let check = 0; check < BUSY_CHECKS; check += 1) {
    if (check > 0) await step.sleep(`${id}-wait-${check}`, BUSY_WAIT);
    const answer = await step.run(check === 0 ? id : `${id}-${check}`, () => runEffect(ask));
    if (!answer.ok && answer.reason === "stale") {
      await step.run(`${id}-stale`, () => runEffect(endRun(run, runId, "failed", answer.message)));
      throw new Stale(answer.message);
    }
    if (answer.ok || answer.reason !== "busy") return answer;
  }
  throw new NonRetriableError(`${still} after ${BUSY_CHECKS} checks`);
}

const setCopy = (steps: RunSteps, id: string, machine: MachineRef, expect: ExpectedCopy, want: WantedCopy, still: string) =>
  settle(steps, id, setVolumeCopy(steps.run, steps.runId, machine, expect, want), still);

/** A step whose refusal ends what the run is doing; the caller decides whether that fails the run or releases it. */
async function mustSet(steps: RunSteps, id: string, machine: MachineRef, expect: ExpectedCopy, want: WantedCopy, still: string) {
  const answer = await setCopy(steps, id, machine, expect, want, still);
  if (!answer.ok) throw new NonRetriableError(answer.message);
  return answer.copy;
}

/** A slot holding a snapshot: what a release after the cut expects of A, the slot keeping the stopped Container. */
type SlotAt = Extract<VolumeCopy, { kind: "slot" }> & { readonly newest: Snapshot };

const slotAt = (copy: VolumeCopy | null): SlotAt | null =>
  copy?.kind === "slot" && copy.newest !== null ? { ...copy, newest: copy.newest } : null;

const pulled = (copy: VolumeCopy | null, target: Snapshot) =>
  copy?.kind === "slot" && copy.pull === null && copy.newest?.guid === target.guid;

const MAX_DRIFTS = 3;

async function askPullFollowingDrift(steps: RunSteps, id: string, B: MachineRef, copy: VolumeCopy | null, want: WantedCopy, still: string) {
  let expect = expectOf(copy);
  for (let drift = 0; ; drift += 1) {
    const answer = await setCopy(steps, drift === 0 ? id : `${id}-drift-${drift}`, B, expect, want, still);
    if (answer.ok) return answer.copy;
    if (answer.reason !== "mismatch" || answer.copy?.kind !== "slot" || drift === MAX_DRIFTS) throw new NonRetriableError(answer.message);
    expect = expectOf(answer.copy);
  }
}

/**
 * B pulls `target` from A and the run asks again until the pull lands. Every check resends the same request: it finishes
 * a landed pull, leaves a running one alone, and resumes one a restart of B stopped.
 */
async function pull(steps: RunSteps, prefix: string, ctx: { readonly B: MachineRef; readonly copy: VolumeCopy | null; readonly from: string; readonly target: Snapshot }) {
  const { step, run } = steps;
  const { B, target } = ctx;
  const slot = copyName(run.volumeName, B.name);
  const want: WantedCopy = { kind: "mirror", from: ctx.from, target: target.name, drain: DRAIN_MS };
  const still = `${slot} is still busy`;
  let copy = await askPullFollowingDrift(steps, `${prefix}-pull`, B, ctx.copy, want, still);
  for (let check = 0; ; check += 1) {
    if (pulled(copy, target)) return copy;
    const failure = copy?.kind === "slot" ? copy.pull?.failure ?? null : null;
    if (failure !== null) throw new NonRetriableError(`${slot} could not receive ${target.name}: ${failure}`);
    const wait = PATIENCE_WAITS[check];
    if (wait === undefined) throw new NonRetriableError(`${slot} has not finished ${target.name} after ${PATIENCE}`);
    // A landed pull still verifying keeps `pull` set, so only an idle copy skips the wait.
    const idle = copy?.kind === "slot" && copy.pull === null;
    // Ids apart from the 5 s checks' `-wait-`/`-repull-`, so a run they started replays none of them into this schedule.
    if (!idle) await step.sleep(`${prefix}-backoff-${check}`, `${wait}s`);
    copy = await askPullFollowingDrift(steps, `${prefix}-ask-${check}`, B, copy, want, still);
  }
}

/**
 * Warm rounds: A snapshots `w-<epoch>-<round>` and B pulls it, until B already holds A's newest or the rounds run out.
 * A takes no snapshot when nothing was written, so its newest is then what B pulls.
 */
async function runRounds(steps: RunSteps, ctx: { readonly A: AnsweredMember; readonly B: MachineRef; readonly copy: VolumeCopy | null; readonly epoch: number }) {
  const { A, B, epoch } = ctx;
  let copy = ctx.copy;
  for (let round = 0; round < MAX_ROUNDS; round += 1) {
    const prefix = `03-r${round}`;
    const warmed = await mustSet(steps, `${prefix}-snapshot`, A.machine, { kind: "writer", epoch: epochOf(A) }, {
      kind: "snapshot",
      name: `w-${epoch}-${round}`,
    }, `${A.machine.name} is still busy with ${steps.run.volumeName}`);
    const target = warmed?.newest ?? null;
    if (target === null) throw new NonRetriableError(`${steps.run.volumeName}'s writer on ${A.machine.name} made no snapshot`);
    if (pulled(copy, target)) return { rounds: round, copy };
    copy = await pull(steps, prefix, { B, copy, from: A.address, target });
  }
  return { rounds: MAX_ROUNDS, copy };
}

async function mirrorVolume(steps: RunSteps, phase: PhaseOf<"mirror"> | PhaseOf<"sync">, epoch: number) {
  const { run } = steps;
  const A = phase.writer;
  const target = phase.kind === "mirror" ? phase.target : phase.mirror;
  const B = target.machine;
  let copy = target.view.copy;
  try {
    if (phase.kind === "sync") {
      await mustSet(steps, "03-reset-forget-mirror", B, expectOf(copy), { kind: "forget" }, `${B.name} is still busy with ${run.volumeName}`);
      await mustSet(steps, "03-reset-forget-writer", A.machine, { kind: "writer", epoch: epochOf(A) }, { kind: "forget" }, `${A.machine.name} is still busy with ${run.volumeName}`);
      copy = null;
    }
    const { rounds } = await runRounds(steps, { A, B, copy, epoch });
    await finish(steps, "05-finish");
    return { runId: run.id, rounds };
  } catch (error) {
    return stopped(steps, "05-failed", error instanceof Error ? error : new Error(String(error)));
  }
}

async function deleteMirror(steps: RunSteps, phase: PhaseOf<"delete_mirror">) {
  const { run } = steps;
  try {
    for (const slot of phase.destroy) {
      await mustSet(steps, `03-forget-${slot.machine.name}`, slot.machine, expectOf(slot.view.copy), { kind: "forget" }, `${slot.machine.name} is still busy with ${run.volumeName}`);
    }
    if (phase.forget !== null) {
      const writer = phase.forget;
      await mustSet(steps, "04-forget", writer.machine, { kind: "writer", epoch: epochOf(writer) }, { kind: "forget" }, `${writer.machine.name} is still busy with ${run.volumeName}`);
    }
  } catch (error) {
    return stopped(steps, "05-failed", error instanceof Error ? error : new Error(String(error)));
  }
  await finish(steps, "05-finish");
  return { runId: run.id, destroyed: phase.destroy.map((slot) => slot.machine.name) };
}

/** Restore makes the one copy left the writer; it starts no Container, so the next Deploy runs the Service on it. */
async function restoreVolume(steps: RunSteps, phase: PhaseOf<"restore">, epoch: number) {
  const { run } = steps;
  const source = phase.from;
  const from = source.machine;
  const copy = source.view.copy;
  const at = copy?.newest ?? null;
  if (copy === null || at === null) return fail(steps, "03-failed", `${copyName(run.volumeName, from.name)} holds no snapshot to restore`);
  const keep = copy.kind === "root" ? copy.epoch : epoch;
  try {
    await mustSet(steps, "03-restore", from, expectOf(copy), { kind: "writer", at: at.guid, epoch: keep, start: null }, `${from.name} is still busy with ${run.volumeName}`);
  } catch (error) {
    return stopped(steps, "03-failed", error instanceof Error ? error : new Error(String(error)));
  }
  const lostAfter = phase.lostAfter;
  const message = lostAfter === null
    ? `${run.volumeName} restored on ${from.name}`
    : `${run.volumeName} restored on ${from.name} from ${new Date(lostAfter.created_unix_seconds * 1000).toISOString()}; writes after that time are lost`;
  await finish(steps, "04-finish", message);
  return { runId: run.id, restored: from.name };
}

/** An old copy takes writes no other copy will see, so the run seals it before anything else and asks for a rerun. */
async function demoteVolume(steps: RunSteps, phase: PhaseOf<"demote">, epoch: number) {
  const { run } = steps;
  const old = phase.old.machine;
  try {
    await mustSet(steps, "03-demote", old, { kind: "writer", epoch: epochOf(phase.old) }, {
      kind: "mirror",
      from: null,
      target: `f-${epoch}`,
      drain: DRAIN_MS,
    }, `${old.name} is still stopping ${run.volumeName}'s Service`);
  } catch (error) {
    return stopped(steps, "03-failed", error instanceof Error ? error : new Error(String(error)));
  }
  const message = `${copyName(run.volumeName, old.name)} was older than ${copyName(run.volumeName, phase.writer.machine.name)} and is now read-only; `
    + `writes to it since ${phase.writer.machine.name} took over are lost. Run this again`;
  await steps.step.run("04-finish", () => steps.runEffect(endRun(run, steps.runId, "failed", message)));
  return { runId: run.id, demoted: old.name };
}

/**
 * Release ends whatever a Move left in flight before its commit. A cut that stopped on a read-only root starts its
 * Container again; a writer whose Move reached B drops the Container A kept; with only slots left, the one keeping the
 * cut's Container becomes the writer again once every other copy at its snapshot is gone, so a start B was sent earlier
 * refuses on its expect.
 */
async function releaseVolume(steps: RunSteps, phase: PhaseOf<"release">, epoch: number) {
  const { run } = steps;
  const { from } = phase;
  const kept = async (id: string, machines: readonly MachineRef[]) =>
    steps.step.run(id, () => steps.runEffect(findKept(run, steps.runId, machines)));
  const resume = async (id: string, member: AnsweredMember, containerId: ContainerId | null) => {
    const copy = member.view.copy;
    const at = copy?.newest ?? null;
    if (copy?.kind !== "root" || at === null) return;
    await mustSet(steps, id, member.machine, expectOf(copy), {
      kind: "writer",
      at: at.guid,
      epoch,
      start: containerId === null ? null : { kind: "kept", container_id: containerId },
    }, `${member.machine.name} is still starting ${run.volumeName}'s Service`);
  };
  try {
    if (from.kind === "cut" || from.kind === "writer") {
      const root = from.kind === "cut" ? from.root : from.writer;
      if (from.kind === "cut" && (root.view.copy?.newest ?? null) === null) {
        throw new NonRetriableError(`${run.volumeName} on ${root.machine.name} stopped before its first snapshot; volume move ${run.volumeName} --to <another server> again`);
      }
      const holder = await kept("03-holder", [root.machine]);
      await resume("04-resume", root, holder?.containerId ?? null);
      if (from.kind === "writer") {
        for (const slot of from.behind) {
          const old = await kept(`05-kept-${slot.machine.name}`, [slot.machine]);
          if (old !== null) await steps.step.run(`05-remove-${slot.machine.name}`, () => steps.runEffect(removeContainer(run, steps.runId, slot.machine, old.containerId)));
        }
      }
      await finish(steps, "06-finish");
      return { runId: run.id, released: root.machine.name };
    }
    const source = await kept("03-holder", from.slots.map((slot) => slot.machine));
    const owner = source === null ? undefined : from.slots.find((slot) => slot.machine.id === source.machine.id);
    const cut = slotAt(owner?.view.copy ?? null);
    if (source === null || owner === undefined || cut === null) {
      const held = from.slots.find((slot) => slotAt(slot.view.copy) !== null)?.machine.name ?? "<server>";
      return fail(steps, "03-no-source", `${run.volumeName} has no writer and no stopped Service on its copies; ployz volume restore ${run.volumeName} --from ${held}`);
    }
    const A = owner.machine;
    const at = cut.newest;
    // SAFETY: the run's kind is release, so its args are release's.
    const { plan } = run.args as VolumeRunArgs["release"];
    const changed = () => new NonRetriableError(
      `${run.volumeName}'s copies changed since this release was planned; nothing was forgotten and ${run.volumeName}'s Service stays stopped on ${A.name}. `
        + `Run ployz volume release ${run.volumeName} again`,
    );
    if (plan !== undefined && !plan.sources.includes(A.id)) throw changed();
    const doomed: Array<{ machine: MachineRef; expect: ExpectedCopy }> = [];
    for (const { machine } of from.slots.filter((slot) => slot !== owner)) {
      const now = await steps.step.run(`04-inspect-${machine.name}`, () => steps.runEffect(inspectCopy(run, steps.runId, machine)));
      if (now?.kind === "root") {
        throw new NonRetriableError(
          `${copyName(run.volumeName, machine.name)} ${now.readonly ? "is a read-only copy mid-cut" : `writes at a snapshot other than ${at.name}`}; `
            + `${run.volumeName}'s Service stays stopped on ${A.name}`,
        );
      }
      if (now?.kind === "slot" && now.newest?.guid === at.guid) doomed.push({ machine, expect: expectOf(now) });
    }
    if (plan !== undefined && doomed.some(({ machine }) => !plan.forgets.includes(machine.id))) throw changed();
    for (const { machine, expect } of doomed) {
      await mustSet(steps, `04-forget-${machine.name}`, machine, expect, { kind: "forget" }, `${machine.name} is still busy with ${run.volumeName}`);
    }
    await mustSet(steps, "05-resume", A, expectOf(cut), {
      kind: "writer",
      at: at.guid,
      epoch,
      start: { kind: "kept", container_id: source.containerId },
    }, `${A.name} is still starting ${run.volumeName}'s Service`);
    await finish(steps, "06-finish");
    return { runId: run.id, released: A.name };
  } catch (error) {
    return stopped(steps, "06-failed", error instanceof Error ? error : new Error(String(error)));
  }
}

/** A release of a committed Move goes forward: B starts the Service at G, then the stopped Containers elsewhere go. */
async function finishRelease(steps: RunSteps, members: readonly Member[], current: MachineId) {
  const { step, runEffect, run, runId } = steps;
  const B = members.find((member): member is AnsweredMember => member.answered && member.machine.id === current);
  const copy = B?.view.copy ?? null;
  const at = copy?.newest ?? null;
  if (!members.some((member) => member.machine.id === current)) {
    const target = await step.run("02-target", () => runEffect(laneTarget(run)));
    const held = members.find((member): member is AnsweredMember => member.answered && (member.view.copy?.newest ?? null) !== null)?.machine.name;
    const lost = `${run.volumeName}'s writer was on ${target ?? current} (removed)`;
    return fail(steps, "02-failed", held === undefined
      ? `${lost}, and no copy left holds its data`
      : `${lost}. Restore from ${held}'s copy: ployz volume restore ${run.volumeName} --from ${held}`);
  }
  if (B === undefined || at === null) return fail(steps, "02-failed", `${run.volumeName}'s Move committed, but its new copy is gone`);
  const { epoch } = await step.run("02-epoch", () => runEffect(takeEpoch(run, runId, observedEpoch(members))));
  try {
    const started = await settle(steps, "03-start", commitAndStart(run, runId, {
      to: B.machine,
      from: null,
      expect: expectOf(copy),
      at: at.guid,
      epoch: copy?.kind === "root" ? copy.epoch : epoch,
    }), `${B.machine.name} is still starting ${run.volumeName}'s Service`);
    if (!started.ok) throw new NonRetriableError(started.message);
    const others = members.filter((member) => member !== B).map((member) => member.machine);
    await step.run("04-remove", () => runEffect(removeKept(run, runId, others)));
  } catch (error) {
    return stopped(steps, "05-failed", error instanceof Error ? error : new Error(String(error)));
  }
  await finish(steps, "05-finish");
  return { runId: run.id, moved: B.machine.name };
}

/** B already writes at G, so the Move lacks only B's Service and A's kept Container gone; A is the slot keeping it. */
async function landVolume(steps: RunSteps, phase: PhaseOf<"landed">) {
  const { step, runEffect, run, runId } = steps;
  const source = await step.run("02-source", () => runEffect(findKept(run, runId, phase.behind.map((member) => member.machine))));
  const writer = source === null ? undefined : phase.behind.find((member) => member.machine.id === source.machine.id);
  if (writer === undefined) {
    const refusal = alreadyOn(run.volumeName, phase.target.machine.name);
    await step.run("02-refuse", () => runEffect(refuseRun(run, runId, refusal)));
    return { runId: run.id, refused: refusal };
  }
  return moveVolume(steps, { kind: "move", writer, target: phase.target, start: "start" }, epochOf(phase.target));
}

/**
 * A Move: warm rounds, the image, the cut on A, B's pull of the cut's snapshot G, then the commit that starts B, and
 * A's stopped Container removed. Past the rounds a failure releases: back to A before the commit, forward to B after.
 */
async function moveVolume(steps: RunSteps, phase: PhaseOf<"move">, epoch: number) {
  const { step, runEffect, run, runId } = steps;
  const A = phase.writer.machine;
  const B = phase.target.machine;
  const name = run.volumeName;
  let source = phase.writer.view.copy;
  let copy = phase.target.view.copy;

  if (phase.start === "rounds") {
    const found = await step.run("02-service", () => runEffect(findHolder(run, runId, A)));
    if (!found.ok) {
      await step.run("02-refuse", () => runEffect(refuseRun(run, runId, found.refusal)));
      return { runId: run.id, refused: found.refusal };
    }
    try {
      ({ copy } = await runRounds(steps, { A: phase.writer, B, copy, epoch }));
    } catch (error) {
      return stopped(steps, "03-failed", error instanceof Error ? error : new Error(String(error)));
    }
  }

  try {
    if (phase.start === "rounds" || phase.start === "cut") {
      const imaged = await step.run("04-image", () => runEffect(findHolder(run, runId, A).pipe(
        Effect.tap((found) => (found.ok ? copyImage(run, runId, A, found.holder, B) : Effect.void)),
      )));
      if (!imaged.ok) throw new NonRetriableError(imaged.refusal.message);
      source = await mustSet(steps, "05-cut", A, { kind: "writer", epoch: epochOf(phase.writer) }, {
        kind: "mirror",
        from: null,
        target: `f-${epoch}`,
        drain: DRAIN_MS,
      }, `${A.name} is still stopping ${name}'s Service`);
    }
    const cut = slotAt(source);
    if (cut === null) throw new NonRetriableError(`${copyName(name, A.name)} holds no final snapshot`);
    if (phase.start !== "start") copy = await pull(steps, "06-final", { B, copy, from: phase.writer.address, target: cut.newest });
    const started = await settle(steps, "07-start", commitAndStart(run, runId, {
      to: B,
      from: A,
      expect: expectOf(copy),
      at: cut.newest.guid,
      epoch: copy?.kind === "root" ? copy.epoch : epoch,
    }), `${B.name} is still starting ${name}'s Service`);
    if (!started.ok) throw new NonRetriableError(started.message);
    await step.run("08-remove", () => runEffect(removeKept(run, runId, [A])));
  } catch (error) {
    return release(steps, "09-failed", error instanceof Error ? error : new Error(String(error)));
  }
  await finish(steps, "09-finish");
  return { runId: run.id, moved: B.name };
}

/**
 * Seed copies one restore point of this Volume into a new Volume's slot on the same Machine, then lands it as an
 * unregistered root, so the Deploy that follows adopts it. The Machine copies and lands; the run resends the same
 * request until the copy it answers is a root.
 */
async function seedVolume({ step, runEffect, run, runId }: RunSteps, phase: PhaseOf<"seed">) {
  // SAFETY: the run's kind is seed, so its args are seed's.
  const args = run.args as VolumeRunArgs["seed"];
  const source = phase.source.machine;
  const failed = async (id: string, message: string): Promise<never> => {
    await step.run(id, () => runEffect(failRun(run, runId, message)));
    throw new NonRetriableError(message);
  };

  for (let poll = 0; ; poll += 1) {
    if (poll > 0) {
      const wait = PATIENCE_WAITS[poll - 1];
      if (wait === undefined) return failed("03-too-long", `${args.into.name} has not finished copying after ${PATIENCE}`);
      await step.sleep(`03-backoff-${poll}`, `${wait}s`);
    }
    // Ids apart from the 5 s checks' `03-wait-`/`03-seed-`, as in `pull`.
    const copy = await step.run(`03-ask-${poll}`, () => runEffect(sendSwitch(run, runId, source, {
      command: "seed_volume",
      payload: { name: run.dockerVolume, into: args.into.dockerVolume, guid: args.guid },
    }).pipe(
      Effect.map((view) => view.copy),
      Effect.catchIf((error) => error instanceof SwitchAttemptError && error.reason === "busy", () => Effect.succeed(null)),
    )));
    if (copy?.kind === "root") break;
    const failure = copy?.kind === "slot" ? copy.pull?.failure : null;
    if (failure) return failed("03-failed", `${args.into.name} could not copy ${args.name}: ${failure}`);
  }

  await step.run("04-finish", () => runEffect(endRun(run, runId, "done", `${args.into.name} copied from ${run.volumeName} on ${source.name}`)));
  return { runId: run.id, seeded: args.into.name };
}

async function sweepOrphans(step: Pick<PloyzStepTools, "run">, runEffect: EffectRunner) {
  const organizations = await step.run("list-organizations", () => runEffect(organizationsWithMachines()));
  let orphanRuns = 0;
  for (const organizationId of organizations) {
    const started = await step.run(`orphans-${organizationId}`, () =>
      runEffect(cleanOrphans(organizationId)));
    orphanRuns += started.length;
  }
  return { orphanRuns };
}

/** What a run's `inngest/function.finished` event says to close its row with: the failure it ended on, or none. */
export const finishedVolumeRun = <Input>(event: Input) => {
  const finished = decodeInngestEnvelope(inngestFunctionFinishedEnvelopeSchema)(event);
  const error = finished.data.error;
  return { functionId: finished.data.function_id, runId: finished.data.run_id, end: { error: error === undefined || error === null ? null : error.message } };
};

const isRunVolume = (functionId: string) =>
  functionId === RUN_VOLUME_FUNCTION_ID || functionId.endsWith(`-${RUN_VOLUME_FUNCTION_ID}`);

const lifecycle = attemptLifecycle({
  run: {
    id: RUN_VOLUME_FUNCTION_ID,
    triggers: [{ event: volumeRunRequestedEventType }],
    cancelOn: [{ event: volumeRunTakenEventType, match: "data.runId" }],
    handler: runVolume,
  },
  cancelId: "cancel-volume-run",
  closeRun: (runId, end) => closeVolumeRun(runId, end),
  sweep: {
    id: "close-stale-volume-runs",
    closeStale: () => closeStaleVolumeRuns(inngestRunStatus),
    afterSweep: sweepOrphans,
  },
});

export const createCloseFinishedVolumeRun = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "close-finished-volume-run",
      retries: 3,
      triggers: [{
        event: inngestFunctionFinishedEventType,
        if: `event.data.function_id == '${RUN_VOLUME_FUNCTION_ID}' || event.data.function_id == 'ployz-cloud-${RUN_VOLUME_FUNCTION_ID}'`,
      }],
    },
    async ({ event, step }) => {
      const finished = await step.run("decode-finished", () => finishedVolumeRun(event));
      if (!isRunVolume(finished.functionId)) return { skipped: true };
      return { closed: await step.run("close-run", () => runEffect(closeVolumeRun(finished.runId, finished.end))) };
    },
  );

export const createRunVolume = lifecycle.createRun;
export const createCancelVolumeRun = lifecycle.createCancel;
export const createCloseStaleVolumeRuns = lifecycle.createSweep;
export const createVolumeRunFunctions = (inngest: PloyzInngest) =>
  [...lifecycle.createFunctions(inngest), createCloseFinishedVolumeRun(inngest)] as const;
