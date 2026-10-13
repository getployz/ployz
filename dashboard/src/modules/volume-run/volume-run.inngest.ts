import type { ContainerId, ExpectedCopy, Snapshot, VolumeCopy, WantedCopy } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import { inngestRunStatus, type PloyzInngest, type PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestFunctionFinishedEnvelopeSchema,
  inngestFunctionFinishedEventType,
  volumeRunRequestedEventType,
} from "#/modules/inngest/events";
import { alreadyOn, epochOf, expectOf, observedEpoch, type PlanInput, type Planned, planFromCopies } from "#/modules/volume-run/plan";
import { type AnsweredMember, copyName, type Member, type VolumeRunArgs } from "#/modules/volume-run/volume-run";
import {
  claimVolumeRun,
  cleanOrphanSlots,
  closeStaleVolumeRuns,
  closeVolumeRun,
  type CopyAnswer,
  copyImage,
  DRAIN_MS,
  endRun,
  failRun,
  findHolder,
  findKept,
  type Holder,
  inspectCopy,
  type MachineRef,
  observeMembers,
  organizationsWithMachines,
  refuseRun,
  removeContainer,
  runningOn,
  type RunContext,
  sendSwitch,
  setVolumeCopy,
  startOnTarget,
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
const POLL_INTERVAL = "5s";
const MAX_POLLS = 500;
// Docker decides how long a Container step takes; 120 checks five seconds apart wait out ten minutes of it.
const BUSY_CHECKS = 120;

const VolumeRunRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  volumeId: Schema.String.check(Schema.isNonEmpty()),
  runId: Schema.String.check(Schema.isUUID()),
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

  // SAFETY: step output is the JSON of the Member list, and a Member holds only JSON values.
  const members = (await step.run("01-observe", () => runEffect(observeMembers(run, runId)))) as Member[];
  // SAFETY: the row's kind and args were written together from one VolumeRunInput.
  const input = { kind: run.kind, args: run.args, volumeName: run.volumeName, orphan: run.orphan } as PlanInput;
  const plan = planFromCopies(input, members);
  if (!plan.ok) {
    await step.run("02-refuse", () => runEffect(refuseRun(run, runId, plan.refusal)));
    return { runId: run.id, refused: plan.refusal };
  }
  const steps: RunSteps = { step, runEffect, run, runId };
  const { phase } = plan;
  if (phase.kind === "seed") return seedVolume(steps, phase);
  if (phase.kind === "delete_mirror") return deleteMirror(steps, phase);
  if (phase.kind === "landed") return landVolume(steps, phase);
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

const fail = ({ step, runEffect, run, runId }: RunSteps, id: string, message: string): Promise<never> =>
  step.run(id, () => runEffect(failRun(run, runId, message))).then(() => {
    throw new NonRetriableError(message);
  });

const finish = ({ step, runEffect, run, runId }: RunSteps, id: string, message: string | null = null) =>
  step.run(id, () => runEffect(endRun(run, runId, "done", message)));

/**
 * One SetVolumeCopy. A Machine answers Busy while a Container step or another switch holds the Volume, so the run
 * sleeps and asks again with the same request, which spends no retry.
 */
async function settle({ step, runEffect }: RunSteps, id: string, ask: Effect.Effect<CopyAnswer, Error, AppServices>, still: string) {
  for (let check = 0; check < BUSY_CHECKS; check += 1) {
    if (check > 0) await step.sleep(`${id}-wait-${check}`, POLL_INTERVAL);
    const answer = await step.run(check === 0 ? id : `${id}-${check}`, () => runEffect(ask));
    if (answer.ok || answer.reason !== "busy") return answer;
  }
  throw new NonRetriableError(`${still} after ${BUSY_CHECKS} checks`);
}

const setCopy = (steps: RunSteps, id: string, machine: MachineRef, expect: ExpectedCopy, want: WantedCopy, still: string) =>
  settle(steps, id, setVolumeCopy(steps.run, steps.runId, machine, expect, want), still);

/** A step whose refusal ends what the run is doing; the caller decides whether that fails the run or undoes it. */
async function mustSet(steps: RunSteps, id: string, machine: MachineRef, expect: ExpectedCopy, want: WantedCopy, still: string) {
  const answer = await setCopy(steps, id, machine, expect, want, still);
  if (!answer.ok) throw new NonRetriableError(answer.message);
  return answer.copy;
}

const isWriterAt = (copy: VolumeCopy | null, at: Snapshot) =>
  copy?.kind === "root" && !copy.readonly && copy.newest?.guid === at.guid;

/** A slot holding a snapshot: what an undo after the cut expects of A, the slot keeping the stopped Container. */
type SlotAt = Extract<VolumeCopy, { kind: "slot" }> & { readonly newest: Snapshot };

const slotAt = (copy: VolumeCopy | null): SlotAt | null =>
  copy?.kind === "slot" && copy.newest !== null ? { ...copy, newest: copy.newest } : null;

const pulled = (copy: VolumeCopy | null, target: Snapshot) =>
  copy?.kind === "slot" && copy.pull === null && copy.newest?.guid === target.guid;

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
  let copy = await mustSet(steps, `${prefix}-pull`, B, expectOf(ctx.copy), want, still);
  for (let poll = 0; poll < MAX_POLLS; poll += 1) {
    if (pulled(copy, target)) return copy;
    const failure = copy?.kind === "slot" ? copy.pull?.failure ?? null : null;
    if (failure !== null) throw new NonRetriableError(`${slot} could not receive ${target.name}: ${failure}`);
    // A landed pull still verifying keeps `pull` set, so only an idle copy skips the wait.
    const idle = copy?.kind === "slot" && copy.pull === null;
    if (!idle) await step.sleep(`${prefix}-wait-${poll}`, POLL_INTERVAL);
    copy = await mustSet(steps, `${prefix}-repull-${poll}`, B, expectOf(copy), want, still);
  }
  throw new NonRetriableError(`${slot} is still receiving ${target.name} after ${MAX_POLLS} checks`);
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
    if (phase.kind === "sync" && phase.full) {
      await mustSet(steps, "03-reset-forget-mirror", B, expectOf(copy), { kind: "forget" }, `${B.name} is still busy with ${run.volumeName}`);
      await mustSet(steps, "03-reset-forget-writer", A.machine, { kind: "writer", epoch: epochOf(A) }, { kind: "forget" }, `${A.machine.name} is still busy with ${run.volumeName}`);
      copy = null;
    }
    const { rounds } = await runRounds(steps, { A, B, copy, epoch });
    await finish(steps, "05-finish");
    return { runId: run.id, rounds };
  } catch (error) {
    return fail(steps, "05-failed", (error instanceof Error ? error.message : String(error)));
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
    return fail(steps, "05-failed", (error instanceof Error ? error.message : String(error)));
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
    return fail(steps, "03-failed", (error instanceof Error ? error.message : String(error)));
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
    return fail(steps, "03-failed", (error instanceof Error ? error.message : String(error)));
  }
  const message = `${copyName(run.volumeName, old.name)} was older than ${copyName(run.volumeName, phase.writer.machine.name)} and is now read-only; `
    + `writes to it since ${phase.writer.machine.name} took over are lost. Run this again`;
  await steps.step.run("04-finish", () => steps.runEffect(endRun(run, steps.runId, "failed", message)));
  return { runId: run.id, demoted: old.name };
}

/**
 * Release ends whatever a Move left in flight. A cut that stopped on a read-only root starts its Container again; a
 * writer whose Move reached B drops the Container A kept; with only slots left, the one keeping the cut's Container
 * becomes the writer again once every other copy at its snapshot is gone.
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
      return fail(steps, "03-no-source", `${run.volumeName} has no writer and no stopped Service on its copies; volume restore ${run.volumeName} --from <server>`);
    }
    const found = await steps.step.run("03-spec", () => steps.runEffect(findHolder(run, steps.runId, owner.machine)));
    // SAFETY: step output is the JSON of a Holder, and a Holder holds only JSON values.
    const holder = found.ok ? found.holder as Holder : null;
    const others = from.slots.filter((slot) => slot !== owner);
    const done = await undoAfterCut(steps, { A: owner.machine, holder, containerId: source.containerId, source: cut, epoch, others });
    await finish(steps, "06-finish");
    return { runId: run.id, released: done === "undone" ? owner.machine.name : null };
  } catch (error) {
    return fail(steps, "06-failed", (error instanceof Error ? error.message : String(error)));
  }
}

/**
 * Undo after the cut. B's slot at G goes first, so a start B was sent earlier refuses on its expect; only then does A
 * become the writer again. B already the writer at G means the Move landed, so it goes forward. Any other root on B
 * leaves A a slot, since A as the writer beside it would be a second writable root.
 */
async function undoAfterCut(
  steps: RunSteps,
  ctx: {
    readonly A: MachineRef;
    readonly holder: Holder | null;
    readonly containerId: ContainerId;
    readonly source: SlotAt;
    readonly epoch: number;
    readonly others: readonly { readonly machine: MachineRef }[];
  },
): Promise<"undone" | "forward"> {
  const { step, runEffect, run, runId } = steps;
  const { A, source } = ctx;
  const at = source.newest;
  const forward = async (B: MachineRef, copy: VolumeCopy | null) => {
    await startOnB(steps, ["13-running", "13-start"], { A, B, holder: ctx.holder, copy, at, epoch: ctx.epoch });
    await step.run("13-remove", () => runEffect(removeContainer(run, runId, A, ctx.containerId)));
    return "forward" as const;
  };
  const otherRoot = (B: MachineRef, copy: Extract<VolumeCopy, { kind: "root" }>) => new NonRetriableError(
    `${copyName(run.volumeName, B.name)} ${copy.readonly ? "is a read-only copy mid-cut" : `writes at a snapshot other than ${at.name}`}; `
      + `${run.volumeName}'s Service stays stopped on ${A.name}`,
  );
  for (const { machine } of ctx.others) {
    const now = await step.run(`13-inspect-${machine.name}`, () => runEffect(inspectCopy(run, runId, machine)));
    if (isWriterAt(now, at)) return forward(machine, now);
    if (now?.kind === "root") throw otherRoot(machine, now);
    if (now?.kind !== "slot" || now.newest?.guid !== at.guid) continue;
    const forgot = await setCopy(steps, `13-forget-${machine.name}`, machine, expectOf(now), { kind: "forget" }, `${machine.name} is still busy with ${run.volumeName}`);
    if (forgot.ok) continue;
    if (isWriterAt(forgot.copy, at)) return forward(machine, forgot.copy);
    if (forgot.copy?.kind === "root") throw otherRoot(machine, forgot.copy);
    throw new NonRetriableError(forgot.message);
  }
  await mustSet(steps, "14-undo", A, expectOf(source), {
    kind: "writer",
    at: at.guid,
    epoch: ctx.epoch,
    start: { kind: "kept", container_id: ctx.containerId },
  }, `${A.name} is still starting ${run.volumeName}'s Service`);
  return "undone";
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
 * A Move: warm rounds, the image, the cut on A, B's pull of the cut's snapshot G, B's start, and A's Container removed.
 * Until B is told to start it undoes back to A; from then on it only goes forward.
 */
async function moveVolume(steps: RunSteps, phase: PhaseOf<"move">, epoch: number) {
  const { step, runEffect, run, runId } = steps;
  const A = phase.writer.machine;
  const B = phase.target.machine;
  const name = run.volumeName;
  const forwardOnly = `volume move ${name} --to ${B.name} again continues from there`;
  let source = phase.writer.view.copy;
  let copy = phase.target.view.copy;

  const found = await step.run("02-holder", () => runEffect(findHolder(run, runId, A)));
  if (!found.ok) {
    if (phase.start === "start") return finishOnTarget(steps, phase, null, epoch);
    await step.run("02-refuse", () => runEffect(refuseRun(run, runId, found.refusal)));
    return { runId: run.id, refused: found.refusal };
  }
  // SAFETY: step output is the JSON of a Holder, and a Holder holds only JSON values.
  const holder = found.holder as Holder;

  if (phase.start === "rounds" || phase.start === "cut") {
    try {
      if (phase.start === "rounds") ({ copy } = await runRounds(steps, { A: phase.writer, B, copy, epoch }));
    } catch (error) {
      return fail(steps, "03-failed", (error instanceof Error ? error.message : String(error)));
    }
    try {
      await step.run("04-image", () => runEffect(copyImage(run, runId, A, holder, B)));
      source = await mustSet(steps, "05-cut", A, { kind: "writer", epoch: epochOf(phase.writer) }, {
        kind: "mirror",
        from: null,
        target: `f-${epoch}`,
        drain: DRAIN_MS,
      }, `${A.name} is still stopping ${holder.redactedSpec.name}`);
    } catch (error) {
      return undoBeforeCut(steps, { A, B, holder, epoch, message: (error instanceof Error ? error.message : String(error)) });
    }
  }

  const cut = slotAt(source);
  if (cut === null) return fail(steps, "06-failed", `${copyName(name, A.name)} holds no final snapshot`);
  if (phase.start !== "start") {
    try {
      copy = await pull(steps, "06-final", { B, copy, from: phase.writer.address, target: cut.newest });
    } catch (error) {
      return undoMove(steps, { A, B, holder, source: cut, epoch, message: (error instanceof Error ? error.message : String(error)) });
    }
  }
  return finishOnTarget(steps, { ...phase, target: { ...phase.target, view: { ...phase.target.view, copy } } }, holder, epoch, forwardOnly);
}

/** Steps 5 and 6: B becomes the writer at G and starts the Service, then A's kept Container goes. */
async function finishOnTarget(steps: RunSteps, phase: PhaseOf<"move">, holder: Holder | null, epoch: number, forwardOnly?: string) {
  const { step, runEffect, run, runId } = steps;
  const A = phase.writer.machine;
  const B = phase.target.machine;
  const copy = phase.target.view.copy;
  const at = phase.writer.view.copy?.kind === "slot" ? phase.writer.view.copy.newest : null;
  const message = forwardOnly ?? `volume move ${run.volumeName} --to ${B.name} again continues from there`;
  try {
    await startOnB(steps, ["07-running", "08-start"], { A, B, holder, copy, at: at ?? copy?.newest ?? null, epoch });
    const kept = holder ?? (await step.run("09-kept", () => runEffect(findKept(run, runId, [A]))));
    if (kept !== null) await step.run("09-remove", () => runEffect(removeContainer(run, runId, A, kept.containerId)));
  } catch (error) {
    return fail(steps, "09-stopped", `${(error instanceof Error ? error.message : String(error))}; ${message}`);
  }
  await finish(steps, "10-finish");
  return { runId: run.id, moved: B.name };
}

/**
 * B runs the Service before anything removes A's kept Container, so a start that fails on B never leaves the Service
 * down on both Machines. A B that writes but runs nothing is sent the same start again, under the same creation key.
 */
async function startOnB(
  steps: RunSteps,
  [runningId, startId]: readonly [string, string],
  ctx: { readonly A: MachineRef; readonly B: MachineRef; readonly holder: Holder | null; readonly copy: VolumeCopy | null; readonly at: Snapshot | null; readonly epoch: number },
) {
  const { step, runEffect, run, runId } = steps;
  const { A, B, holder, copy, at } = ctx;
  const running = await step.run(runningId, () => runEffect(runningOn(run, runId, B)));
  if (running) return;
  if (holder === null || at === null) throw new NonRetriableError(`${run.volumeName} has no Service container on ${A.name} to start on ${B.name}`);
  const started = await settle(steps, startId, startOnTarget(run, runId, {
    from: A,
    holder,
    to: B,
    expect: expectOf(copy),
    at: at.guid,
    epoch: copy?.kind === "root" ? copy.epoch : ctx.epoch,
  }), `${B.name} is still starting ${holder.redactedSpec.name}`);
  if (!started.ok) throw new NonRetriableError(started.message);
}

/**
 * Before the cut A is still the writer, so its kept Container starts again; a cut that landed meanwhile undoes as after
 * the cut. A takes the run's epoch, so a cut this run sent that lands late expects the old one and refuses.
 */
async function undoBeforeCut(steps: RunSteps, ctx: { readonly A: MachineRef; readonly B: MachineRef; readonly holder: Holder; readonly epoch: number; readonly message: string }) {
  const { step, runEffect, run, runId } = steps;
  const { A, holder } = ctx;
  try {
    const now = await step.run("12-inspect", () => runEffect(inspectCopy(run, runId, A)));
    const cut = slotAt(now);
    if (cut !== null) return undoMove(steps, { ...ctx, source: cut });
    const at = now?.newest ?? null;
    if (now?.kind !== "root" || at === null) throw new NonRetriableError(`${copyName(run.volumeName, A.name)} is gone`);
    await mustSet(steps, "13-undo", A, expectOf(now), {
      kind: "writer",
      at: at.guid,
      epoch: ctx.epoch,
      start: { kind: "kept", container_id: holder.containerId },
    }, `${A.name} is still starting ${holder.redactedSpec.name}`);
  } catch (error) {
    if (error instanceof NonRetriableError) return fail(steps, "13-failed", `${error.message}; volume release ${run.volumeName} when it is back`);
    return fail(steps, "13-unanswered", `${A.name} did not answer; volume release ${run.volumeName} when it is back`);
  }
  return fail(steps, "14-undone", ctx.message);
}

async function undoMove(
  steps: RunSteps,
  ctx: { readonly A: MachineRef; readonly B: MachineRef; readonly holder: Holder; readonly source: SlotAt; readonly epoch: number; readonly message: string },
) {
  const { run } = steps;
  try {
    const done = await undoAfterCut(steps, { A: ctx.A, holder: ctx.holder, containerId: ctx.holder.containerId, source: ctx.source, epoch: ctx.epoch, others: [{ machine: ctx.B }] });
    if (done === "forward") {
      await finish(steps, "15-finish");
      return { runId: run.id, moved: ctx.B.name };
    }
  } catch (error) {
    return fail(steps, "15-unanswered", `${(error instanceof Error ? error.message : String(error))}; volume release ${run.volumeName} when it is back`);
  }
  return fail(steps, "15-undone", ctx.message);
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
    if (poll === MAX_POLLS) return failed("03-too-long", `${args.into.name} is still copying after ${MAX_POLLS} checks`);
    if (poll > 0) await step.sleep(`03-wait-${poll}`, POLL_INTERVAL);
    const copy = await step.run(`03-seed-${poll}`, () => runEffect(sendSwitch(run, runId, source, {
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
      runEffect(cleanOrphanSlots(organizationId)));
    orphanRuns += started.length;
  }
  return { orphanRuns };
}

const isRunVolume = (functionId: string) =>
  functionId === RUN_VOLUME_FUNCTION_ID || functionId.endsWith(`-${RUN_VOLUME_FUNCTION_ID}`);

const lifecycle = attemptLifecycle({
  run: {
    id: RUN_VOLUME_FUNCTION_ID,
    triggers: [{ event: volumeRunRequestedEventType }],
    singleton: { key: "event.data.volumeId", mode: "skip" },
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
      const finished = await step.run("decode-finished", () =>
        decodeInngestEnvelope(inngestFunctionFinishedEnvelopeSchema)(event));
      if (!isRunVolume(finished.data.function_id)) return { skipped: true };
      const error = finished.data.error;
      const end = { error: error === undefined || error === null ? null : error.message };
      return { closed: await step.run("close-run", () => runEffect(closeVolumeRun(finished.data.run_id, end))) };
    },
  );

export const createRunVolume = lifecycle.createRun;
export const createCancelVolumeRun = lifecycle.createCancel;
export const createCloseStaleVolumeRuns = lifecycle.createSweep;
export const createVolumeRunFunctions = (inngest: PloyzInngest) =>
  [...lifecycle.createFunctions(inngest), createCloseFinishedVolumeRun(inngest)] as const;
