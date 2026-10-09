import type { Snapshot, SnapshotGuid, VolumeSwitchRequest } from "@ployz/sdk";
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
import { participantsOf, type Planned, planFromCopies, undoClearsTarget } from "#/modules/volume-run/plan";
import { copyName, type Member } from "#/modules/volume-run/volume-run";
import {
  claimVolumeRun,
  cleanOrphanSlots,
  closeStaleVolumeRuns,
  closeVolumeRun,
  copyImage,
  endRun,
  failRun,
  findHolder,
  type Holder,
  type MachineRef,
  observeMembers,
  organizationsWithMachines,
  refuseRun,
  type RunContext,
  sendSwitch,
  startHanded,
  SwitchAttemptError,
  type SwitchOptions,
  takeLease,
} from "#/modules/volume-run/volume-run.server";
import { runInngestEffect } from "#/server/run.server";
import type { AppServices } from "#/server/runtime.server";

type StepTools = Pick<PloyzStepTools, "run" | "sleep">;
type EffectRunner = typeof runInngestEffect;

export const RUN_VOLUME_FUNCTION_ID = "run-volume";
// A Volume written faster than it ships never converges, so the rounds are capped.
export const MAX_ROUNDS = 3;
const POLL_INTERVAL = "5s";
const MAX_POLLS = 500;
// Cloud's own patience for Promote's task on the target: 120 checks five seconds apart, ten minutes.
const PROMOTE_CHECKS = 120;
// Docker decides how long a Container step takes; 120 checks five seconds apart wait out ten minutes of it.
const CONTAINER_CHECKS = 120;

const VolumeRunRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  volumeId: Schema.String.check(Schema.isNonEmpty()),
  runId: Schema.String.check(Schema.isUUID()),
});

type Pos = { readonly seq: number; readonly round: number; readonly sub: number };
type At = (pos: Pos) => { lease: number; pos: Pos };
type MirrorAt = (pos: Pos) => { switch: ReturnType<At>; name: string };
type PhaseOf<K> = Extract<Extract<Planned, { ok: true }>["phase"], { kind: K }>;
type RunSteps = {
  readonly step: StepTools;
  readonly runEffect: EffectRunner;
  readonly run: RunContext;
  readonly runId: string;
  readonly at: At;
  readonly mirrorRequest: MirrorAt;
};
type Asked<T> =
  | { readonly ok: true; readonly working: false; readonly value: T }
  | { readonly ok: true; readonly working: true }
  | { readonly ok: false; readonly message: string };
type Received = { readonly ok: true; readonly guid: SnapshotGuid; readonly name: string } | { readonly ok: false; readonly message: string };
type Newest = { readonly name: string; readonly guid: SnapshotGuid; readonly created_unix_seconds: number } | null;

const newestOf = (snapshot: Snapshot | null | undefined): Newest =>
  snapshot === null || snapshot === undefined
    ? null
    : { name: snapshot.name, guid: snapshot.guid, created_unix_seconds: snapshot.created_unix_seconds };

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
  const plan = planFromCopies({ kind: run.kind, args: run.args, volumeName: run.volumeName, orphan: run.orphan } as Parameters<typeof planFromCopies>[0], members);
  if (!plan.ok) {
    await step.run("02-refuse", () => runEffect(refuseRun(run, runId, plan.refusal)));
    return { runId: run.id, refused: plan.refusal };
  }
  const { phase } = plan;
  const source = phase.kind === "move" ? (phase.start === "close" ? null : phase.writer) : phase.kind === "release" && phase.thaw ? phase.source : null;
  let holder: Holder | null = null;
  if (source !== null) {
    const found = await step.run("02-holder", () => runEffect(findHolder(run, runId, source.machine)));
    if (!found.ok) {
      await step.run("02-refuse", () => runEffect(refuseRun(run, runId, found.refusal)));
      return { runId: run.id, refused: found.refusal };
    }
    holder = found.holder;
  }
  const { lease } = await step.run("02-lease", () => runEffect(takeLease(run, runId, participantsOf(phase))));
  const at = (pos: Pos) => ({ lease, pos });
  const mirrorRequest = (pos: Pos) => ({ switch: at(pos), name: run.dockerVolume });
  const verb = (id: string, effect: ReturnType<typeof sendSwitch>) => step.run(id, () => runEffect(effect.pipe(Effect.asVoid)));
  const steps: RunSteps = { step, runEffect, run, runId, at, mirrorRequest };

  if (phase.kind === "move") return moveVolume(steps, phase, lease, holder);
  if (phase.kind === "release") {
    if (holder !== null) {
      const thaw = sourceRequest(steps, holder, { seq: 13, round: 0, sub: 0 });
      await containerStep(steps, "13-thaw", sendSwitch(run, runId, phase.source.machine, { command: "thaw", payload: thaw }), `${phase.source.machine.name} is still starting ${holder.redactedSpec.name}`);
    }
    if (phase.mirror !== null) {
      const clear = mirrorRequest({ seq: 14, round: 0, sub: 0 });
      await verb("14-clear-final", sendSwitch(run, runId, phase.mirror.machine, { command: "clear_final", payload: clear }));
    }
    await step.run("15-finish", () => runEffect(endRun(run, runId, "done", null)));
    return { runId: run.id, released: phase.source.machine.name };
  }
  if (phase.kind === "delete_mirror") {
    for (const slot of phase.destroy) {
      await verb(`03-destroy-${slot.machine.name}`, sendSwitch(run, runId, slot.machine, {
        command: "destroy_mirror",
        payload: mirrorRequest({ seq: 3, round: 0, sub: 0 }),
      }));
    }
    if (phase.forget !== null) {
      await verb("04-forget", sendSwitch(run, runId, phase.forget.machine, {
        command: "forget_snapshots",
        payload: mirrorRequest({ seq: 4, round: 0, sub: 0 }),
      }));
    }
    for (const member of phase.forgetLease) {
      await verb(`04-forget-lease-${member.machine.name}`, sendSwitch(run, runId, member.machine, {
        command: "forget_lease",
        payload: mirrorRequest({ seq: 4, round: 0, sub: 1 }),
      }));
    }
    await step.run("05-finish", () => runEffect(endRun(run, runId, "done", null)));
    return { runId: run.id, destroyed: phase.destroy.map((slot) => slot.machine.name) };
  }

  const writer = phase.writer;
  const mirror = phase.kind === "mirror" ? phase.target : phase.mirror;
  const A = writer.machine;
  const B = mirror.machine;
  const declare = (id: string, pos: Pos) =>
    verb(id, sendSwitch(run, runId, B, {
      command: "declare_mirror",
      payload: { switch: at(pos), name: run.dockerVolume, refquota_bytes: run.refquotaBytes },
    }));

  if (phase.kind === "mirror" && phase.declare) await declare("03-declare", { seq: 3, round: 0, sub: 0 });
  if (phase.kind === "sync" && phase.full) {
    await verb("03-reset-destroy", sendSwitch(run, runId, B, {
      command: "destroy_mirror",
      payload: mirrorRequest({ seq: 3, round: 0, sub: 0 }),
    }));
    await verb("03-reset-forget", sendSwitch(run, runId, A, {
      command: "forget_snapshots",
      payload: mirrorRequest({ seq: 3, round: 0, sub: 1 }),
    }));
    await declare("03-reset-declare", { seq: 3, round: 0, sub: 2 });
  }

  const rounds = await runRounds(steps, { A, B, writerAddress: writer.address });
  await step.run("05-finish", () => runEffect(endRun(run, runId, "done", null)));
  return { runId: run.id, rounds };
}

const sourceRequest = ({ at, run }: RunSteps, holder: Holder, pos: Pos) =>
  ({ switch: at(pos), name: run.dockerVolume, container_id: holder.containerId });

const serviceRequest = ({ at, run }: RunSteps, holder: Holder, pos: Pos) => ({
  switch: at(pos),
  name: run.dockerVolume,
  namespace: holder.namespace,
  resolved_spec: holder.redactedSpec,
});

const MOVE_STARTS = ["rounds", "handover", "accept", "promote", "start", "close"] as const;

/**
 * One verb whose refusal the Move handles itself: the step records the refusal as its result, and the run throws it
 * from there, so a refused step and a step that ran out of retries reach the same catch.
 */
async function attempt(
  { step, runEffect, run, runId }: RunSteps,
  id: string,
  machine: MachineRef,
  request: Exclude<VolumeSwitchRequest, { command: "inspect_volume_copy" | "inspect_receive" }>,
) {
  const result = await step.run(id, () => runEffect(sendSwitch(run, runId, machine, request, { onRefusal: "throw" }).pipe(
    Effect.map((reply) => ({ ok: true as const, reply })),
    Effect.catchIf((error) => error instanceof NonRetriableError, (error) => Effect.succeed({ ok: false as const, message: error.message })),
  )));
  if (!result.ok) throw new NonRetriableError(result.message);
  return result.reply;
}

/**
 * Freeze's docker stop and the docker start of a Thaw or a handed Start run on the Machine until Docker answers, and
 * the Machine answers Busy meanwhile. The run sleeps and asks again at the same position, which spends no retry.
 */
async function containerStep<T>({ step, runEffect }: RunSteps, id: string, effect: Effect.Effect<T, Error, AppServices>, still: string) {
  for (let check = 0; check < CONTAINER_CHECKS; check += 1) {
    if (check > 0) await step.sleep(`${id}-wait-${check}`, POLL_INTERVAL);
    // SAFETY: step output is the JSON of an Asked<T>, and each T a Container step answers holds only JSON values.
    const asked = (await step.run(check === 0 ? id : `${id}-${check}`, () => runEffect(effect.pipe(
      Effect.map((value): Asked<T> => ({ ok: true, working: false, value })),
      Effect.catchIf((error) => error instanceof SwitchAttemptError && error.reason === "busy", () => Effect.succeed<Asked<T>>({ ok: true, working: true })),
      Effect.catchIf((error) => error instanceof NonRetriableError, (error) => Effect.succeed<Asked<T>>({ ok: false, message: error.message })),
    )))) as Asked<T>;
    if (!asked.ok) throw new NonRetriableError(asked.message);
    if (!asked.working) return asked.value;
  }
  throw new NonRetriableError(`${still} after ${CONTAINER_CHECKS} checks`);
}

/** Steps 05 to 08 can be undone by thawing the source; from 09 on the target holds the only writable future, so a Move only goes forward. */
async function moveVolume(steps: RunSteps, phase: PhaseOf<"move">, lease: number, holder: Holder | null) {
  const { step, runEffect, run, runId, at, mirrorRequest } = steps;
  const A = phase.writer.machine;
  const B = phase.target.machine;
  const name = run.volumeName;
  const from = (start: (typeof MOVE_STARTS)[number]) =>
    phase.start !== "undo" && MOVE_STARTS.indexOf(phase.start) <= MOVE_STARTS.indexOf(start);
  const switchStep = <R extends VolumeSwitchRequest>(id: string, machine: MachineRef, request: R, options?: SwitchOptions) =>
    step.run(id, () => runEffect(sendSwitch(run, runId, machine, request, options)));
  const fail = (id: string, message: string) =>
    step.run(id, () => runEffect(failRun(run, runId, message))).then(() => {
      throw new NonRetriableError(message);
    });
  const forward = `volume move ${name} --to ${B.name} again continues from there`;
  const required = (value: Holder | null) => {
    if (value === null) throw new NonRetriableError(`${name} has no Service container on ${A.name}`);
    return value;
  };

  const undo = async (message: string) => {
    try {
      const thaw = sourceRequest(steps, required(holder), { seq: 13, round: 0, sub: 0 });
      await containerStep(steps, "13-thaw", sendSwitch(run, runId, A, { command: "thaw", payload: thaw }, {
        messages: { precondition: `${name} is handed to ${B.name}; ${forward}` },
      }), `${A.name} is still starting ${required(holder).redactedSpec.name}`);
    } catch {
      return fail("13-unanswered", `${A.name} did not answer; volume move ${name} --to ${B.name} or volume release ${name} when it is back`);
    }
    if (undoClearsTarget(phase)) {
      const clear = mirrorRequest({ seq: 14, round: 0, sub: 0 });
      await switchStep("14-clear-final", B, { command: "clear_final", payload: clear });
    }
    return fail("14-undone", message);
  };

  if (phase.start === "undo") return undo(`${name}'s earlier move was undone; volume move ${name} --to ${B.name} to move it`);

  let guid = phase.guid;
  if (phase.start === "rounds") {
    if (phase.declare) {
      await switchStep("03-declare", B, {
        command: "declare_mirror",
        payload: { switch: at({ seq: 3, round: 0, sub: 0 }), name: run.dockerVolume, refquota_bytes: run.refquotaBytes },
      });
    }
    await runRounds(steps, { A, B, writerAddress: phase.writer.address });
  }
  if (from("start")) await step.run("04-image", () => runEffect(copyImage(run, runId, A, required(holder), B)));

  if (from("handover")) {
    try {
      if (phase.start === "rounds") {
        await attempt(steps, "05-withdraw", A, {
          command: "withdraw",
          payload: sourceRequest(steps, required(holder), { seq: 5, round: 0, sub: 0 }),
        });
        const frozen = await containerStep(steps, "06-freeze", sendSwitch(run, runId, A, {
          command: "freeze",
          payload: sourceRequest(steps, required(holder), { seq: 6, round: 0, sub: 0 }),
        }, { onRefusal: "throw" }), `${A.name} is still stopping ${required(holder).redactedSpec.name}`);
        const writer = frozen.copy?.kind === "root" ? frozen.copy.writer : null;
        if (writer?.phase !== "frozen") throw new NonRetriableError(`${name} on ${A.name} did not freeze`);
        const frozenGuid = writer.guid;
        guid = frozenGuid;
        await sendFinal(steps, { A, B, writerAddress: phase.writer.address, lease, guid: frozenGuid });
      }
      if (guid === null) throw new NonRetriableError(`${name} on ${A.name} has no final snapshot`);
      const handed = guid;
      await attempt(steps, "08-handover", A, {
        command: "hand_over",
        payload: { switch: at({ seq: 8, round: 0, sub: 0 }), name: run.dockerVolume, guid: handed },
      });
    } catch (error) {
      return undo(error instanceof Error ? error.message : String(error));
    }
  }

  try {
    if (from("accept")) {
      if (guid === null) throw new NonRetriableError(`${name} on ${A.name} has no final snapshot`);
      const handed = guid;
      await attempt(steps, "09-accept", B, {
        command: "accept_hand_off",
        payload: { switch: at({ seq: 9, round: 0, sub: 0 }), name: run.dockerVolume, guid: handed },
      });
    }
    if (from("promote")) await promote(steps, B, required(holder));
    if (from("start")) {
      await containerStep(steps, "11-start", startHanded(run, runId, A, required(holder), B, at({ seq: 11, round: 0, sub: 0 })), `${B.name} is still starting ${required(holder).redactedSpec.name}`);
    }
    await attempt(steps, "12-close", A, { command: "close", payload: mirrorRequest({ seq: 12, round: 0, sub: 0 }) });
  } catch (error) {
    return fail("12-stopped", `${error instanceof Error ? error.message : String(error)}; ${forward}`);
  }
  await step.run("12-finish", () => runEffect(endRun(run, runId, "done", null)));
  return { runId: run.id, moved: B.name };
}

/** Promote runs as a task on the target; asking again at the same position reports Busy until the root is writable. */
async function promote(steps: RunSteps, B: MachineRef, holder: Holder) {
  const { step, runEffect, run, runId } = steps;
  for (let check = 0; check < PROMOTE_CHECKS; check += 1) {
    if (check > 0) await step.sleep(`10-promote-wait-${check}`, POLL_INTERVAL);
    const checked = await step.run(`10-promote-${check}`, () => runEffect(
      sendSwitch(run, runId, B, {
        command: "promote",
        payload: serviceRequest(steps, holder, { seq: 10, round: 0, sub: 0 }),
      }, { onRefusal: "throw" }).pipe(
        Effect.map((reply) => ({ ok: true as const, promoted: reply.copy?.kind === "root" && !reply.copy.readonly })),
        Effect.catchIf((error) => error instanceof SwitchAttemptError && error.reason === "busy", () => Effect.succeed({ ok: true as const, promoted: false })),
        Effect.catchIf((error) => error instanceof NonRetriableError, (error) => Effect.succeed({ ok: false as const, message: error.message })),
      ),
    ));
    if (!checked.ok) throw new NonRetriableError(checked.message);
    if (checked.promoted) return;
  }
  throw new NonRetriableError(`${copyName(run.volumeName, B.name)} is still promoting after ${PROMOTE_CHECKS} checks`);
}

/** The frozen source's last snapshot, `f-<lease>`, onto the mirror; Accept later requires exactly this guid. */
async function sendFinal(
  steps: RunSteps,
  ctx: { readonly A: MachineRef; readonly B: MachineRef; readonly writerAddress: string; readonly lease: number; readonly guid: SnapshotGuid },
) {
  const { run, at, mirrorRequest } = steps;
  const { B, guid } = ctx;
  const begun = await attempt(steps, "07-final-0-begin", B, {
    command: "begin_round",
    payload: mirrorRequest({ seq: 7, round: 0, sub: 0 }),
  });
  const base = begun.copy?.newest?.guid ?? null;
  const receive = (id: string, token: string | null) =>
    attempt(steps, id, B, {
      command: "start_receive",
      payload: {
        switch: at({ seq: 7, round: 0, sub: 3 }),
        name: run.dockerVolume,
        from: ctx.writerAddress,
        base,
        target: `f-${ctx.lease}`,
        resume_token: token,
      },
    });
  await receive("07-final-3-receive", null);
  const received = await pollReceive(steps, B, 0, "07-final", receive);
  if (!received.ok) throw new NonRetriableError(received.message);
  if (received.guid !== guid) {
    throw new NonRetriableError(`${copyName(run.volumeName, B.name)} received ${received.guid}, not the frozen ${guid}`);
  }
  await attempt(steps, "07-final-4-prune", B, {
    command: "prune_mirror",
    payload: mirrorRequest({ seq: 7, round: 0, sub: 4 }),
  });
}

async function runRounds(
  steps: RunSteps,
  ctx: { readonly A: MachineRef; readonly B: MachineRef; readonly writerAddress: string },
) {
  const { step, runEffect, run, runId, at, mirrorRequest } = steps;
  const { A, B } = ctx;
  const switchStep = (id: string, machine: MachineRef, request: Parameters<typeof sendSwitch>[3], options?: SwitchOptions) =>
    step.run(id, () => runEffect(sendSwitch(run, runId, machine, request, options).pipe(
      Effect.map((reply) => ("copy" in reply ? { newest: newestOf(reply.copy?.newest) } : { newest: newestOf(null) })),
    )));

  for (let round = 0; round < MAX_ROUNDS; round += 1) {
    const prefix = `04-r${round}`;
    const begun = await switchStep(`${prefix}-0-begin`, B, {
      command: "begin_round",
      payload: mirrorRequest({ seq: 4, round, sub: 0 }),
    });
    const mirrored = begun.newest;
    if (mirrored !== null) {
      const diverged = `${copyName(run.volumeName, B.name)} diverged at ${new Date(mirrored.created_unix_seconds * 1000).toISOString()}; volume sync --full to rebuild`;
      await switchStep(`${prefix}-1-commit`, A, {
        command: "commit_snapshots",
        payload: { switch: at({ seq: 4, round, sub: 1 }), name: run.dockerVolume, mirror_newest: mirrored.guid },
      }, { messages: { precondition: diverged } });
    }
    const warmed = await switchStep(`${prefix}-2-warm`, A, {
      command: "warm_snapshot",
      payload: mirrorRequest({ seq: 4, round, sub: 2 }),
    });
    const target = warmed.newest;
    if (target === null) {
      await step.run(`${prefix}-2-no-snapshot`, () =>
        runEffect(failRun(run, runId, `${run.volumeName}'s writer on ${A.name} made no snapshot`)));
      return round;
    }
    if (mirrored !== null && mirrored.guid === target.guid) return round;

    const receive = (id: string, token: string | null) =>
      switchStep(id, B, {
        command: "start_receive",
        payload: {
          switch: at({ seq: 4, round, sub: 3 }),
          name: run.dockerVolume,
          from: ctx.writerAddress,
          base: mirrored?.guid ?? null,
          target: target.name,
          resume_token: token,
        },
      });
    await receive(`${prefix}-3-receive`, null);
    const received = await pollReceive(steps, B, round, prefix, receive);
    if (!received.ok) {
      await step.run(`${prefix}-3-failed`, () => runEffect(failRun(run, runId, received.message)));
      throw new Error("unreachable: failRun ends the run");
    }

    await switchStep(`${prefix}-4-prune`, B, {
      command: "prune_mirror",
      payload: mirrorRequest({ seq: 4, round, sub: 4 }),
    });
    await switchStep(`${prefix}-5-commit`, A, {
      command: "commit_snapshots",
      payload: { switch: at({ seq: 4, round, sub: 5 }), name: run.dockerVolume, mirror_newest: received.guid },
    });
  }
  return MAX_ROUNDS;
}

async function pollReceive(
  { step, runEffect, run, runId }: RunSteps,
  B: MachineRef,
  round: number,
  prefix: string,
  receive: (id: string, token: string | null) => Promise<object>,
): Promise<Received> {
  const slot = copyName(run.volumeName, B.name);
  for (let poll = 0; poll < MAX_POLLS; poll += 1) {
    await step.sleep(`${prefix}-3-wait-${poll}`, POLL_INTERVAL);
    const status = await step.run(`${prefix}-3-inspect-${poll}`, () =>
      runEffect(sendSwitch(run, runId, B, { command: "inspect_receive", payload: { name: run.dockerVolume, round } })
        .pipe(Effect.map((view) => view.status))));
    switch (status.state) {
      case "running":
        continue;
      case "done":
        return { ok: true, guid: status.newest.guid, name: status.newest.name };
      case "resumable":
        await receive(`${prefix}-3-resume-${poll}`, status.token);
        continue;
      case "failed":
        return { ok: false, message: `${slot} could not receive ${status.target}: ${status.reason}` };
      case "idle":
        return { ok: false, message: `${slot} stopped receiving without a result` };
    }
  }
  return { ok: false, message: `${slot} is still receiving after ${MAX_POLLS} checks` };
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
