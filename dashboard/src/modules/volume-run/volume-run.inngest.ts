import type { Snapshot, SnapshotGuid } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import { inngestRunStatus, type PloyzInngest, type PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestFunctionFinishedEnvelopeSchema,
  inngestFunctionFinishedEventType,
  volumeRunRequestedEventType,
} from "#/modules/inngest/events";
import { planFromCopies } from "#/modules/volume-run/plan";
import { copyName, type Member } from "#/modules/volume-run/volume-run";
import {
  claimVolumeRun,
  cleanOrphanSlots,
  closeStaleVolumeRuns,
  closeVolumeRun,
  endRun,
  failRun,
  type MachineRef,
  observeMembers,
  organizationsWithMachines,
  refuseRun,
  type RunContext,
  sendSwitch,
  type SwitchMessages,
  takeLease,
} from "#/modules/volume-run/volume-run.server";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sleep">;
type EffectRunner = typeof runInngestEffect;

export const RUN_VOLUME_FUNCTION_ID = "run-volume";
// A Volume written faster than it ships never converges, so the rounds are capped.
export const MAX_ROUNDS = 3;
const POLL_INTERVAL = "5s";
const MAX_POLLS = 500;

const VolumeRunRequestedData = Schema.Struct({
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  volumeId: Schema.String.check(Schema.isNonEmpty()),
  runId: Schema.String.check(Schema.isUUID()),
});

type Pos = { readonly seq: number; readonly round: number; readonly sub: number };
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
  const { lease } = await step.run("02-lease", () => runEffect(takeLease(run, runId, members)));
  const at = (pos: Pos) => (notAfter: number) => ({ lease, pos, not_after_unix_seconds: notAfter });
  const mirrorRequest = (pos: Pos) => (notAfter: number) => ({ switch: at(pos)(notAfter), name: run.dockerVolume });
  const verb = (id: string, effect: ReturnType<typeof sendSwitch>) => step.run(id, () => runEffect(effect.pipe(Effect.asVoid)));

  const { phase } = plan;
  if (phase.kind === "delete_mirror") {
    for (const slot of phase.destroy) {
      await verb(`03-destroy-${slot.machine.name}`, sendSwitch(run, runId, slot.machine, (notAfter) => ({
        command: "destroy_mirror",
        payload: mirrorRequest({ seq: 3, round: 0, sub: 0 })(notAfter),
      })));
    }
    if (phase.forget !== null) {
      await verb("04-forget", sendSwitch(run, runId, phase.forget.machine, (notAfter) => ({
        command: "forget_snapshots",
        payload: mirrorRequest({ seq: 4, round: 0, sub: 0 })(notAfter),
      })));
    }
    for (const member of phase.forgetLease) {
      await verb(`04-forget-lease-${member.machine.name}`, sendSwitch(run, runId, member.machine, (notAfter) => ({
        command: "forget_lease",
        payload: mirrorRequest({ seq: 4, round: 0, sub: 1 })(notAfter),
      })));
    }
    await step.run("05-finish", () => runEffect(endRun(run, runId, "done", null)));
    return { runId: run.id, destroyed: phase.destroy.map((slot) => slot.machine.name) };
  }

  const writer = phase.writer;
  const mirror = phase.kind === "mirror" ? phase.target : phase.mirror;
  const A = writer.machine;
  const B = mirror.machine;
  const declare = (id: string, pos: Pos) =>
    verb(id, sendSwitch(run, runId, B, (notAfter) => ({
      command: "declare_mirror",
      payload: { switch: at(pos)(notAfter), name: run.dockerVolume, refquota_bytes: run.refquotaBytes },
    })));

  if (phase.kind === "mirror" && phase.declare) await declare("03-declare", { seq: 3, round: 0, sub: 0 });
  if (phase.kind === "sync" && phase.full) {
    await verb("03-reset-destroy", sendSwitch(run, runId, B, (notAfter) => ({
      command: "destroy_mirror",
      payload: mirrorRequest({ seq: 3, round: 0, sub: 0 })(notAfter),
    })));
    await verb("03-reset-forget", sendSwitch(run, runId, A, (notAfter) => ({
      command: "forget_snapshots",
      payload: mirrorRequest({ seq: 3, round: 0, sub: 1 })(notAfter),
    })));
    await declare("03-reset-declare", { seq: 3, round: 0, sub: 2 });
  }

  const rounds = await runRounds(step, runEffect, run, runId, { A, B, writerAddress: writer.address, at, mirrorRequest });
  await step.run("05-finish", () => runEffect(endRun(run, runId, "done", null)));
  return { runId: run.id, rounds };
}

async function runRounds(
  step: StepTools,
  runEffect: EffectRunner,
  run: RunContext,
  runId: string,
  ctx: {
    readonly A: MachineRef;
    readonly B: MachineRef;
    readonly writerAddress: string;
    readonly at: (pos: Pos) => (notAfter: number) => { lease: number; pos: Pos; not_after_unix_seconds: number };
    readonly mirrorRequest: (pos: Pos) => (notAfter: number) => { switch: ReturnType<ReturnType<typeof ctx.at>>; name: string };
  },
) {
  const { A, B, at, mirrorRequest } = ctx;
  const switchStep = (id: string, machine: MachineRef, build: Parameters<typeof sendSwitch>[3], overrides?: SwitchMessages) =>
    step.run(id, () => runEffect(sendSwitch(run, runId, machine, build, overrides).pipe(
      Effect.map((reply) => ("copy" in reply ? { newest: newestOf(reply.copy?.newest) } : { newest: newestOf(null) })),
    )));

  for (let round = 0; round < MAX_ROUNDS; round += 1) {
    const prefix = `04-r${round}`;
    const begun = await switchStep(`${prefix}-0-begin`, B, (notAfter) => ({
      command: "begin_round",
      payload: mirrorRequest({ seq: 4, round, sub: 0 })(notAfter),
    }));
    const mirrored = begun.newest;
    if (mirrored !== null) {
      const diverged = `${copyName(run.volumeName, B.name)} diverged at ${new Date(mirrored.created_unix_seconds * 1000).toISOString()}; volume sync --full to rebuild`;
      await switchStep(`${prefix}-1-commit`, A, (notAfter) => ({
        command: "commit_snapshots",
        payload: { switch: at({ seq: 4, round, sub: 1 })(notAfter), name: run.dockerVolume, mirror_newest: mirrored.guid },
      }), { precondition: diverged });
    }
    const warmed = await switchStep(`${prefix}-2-warm`, A, (notAfter) => ({
      command: "warm_snapshot",
      payload: mirrorRequest({ seq: 4, round, sub: 2 })(notAfter),
    }));
    const target = warmed.newest;
    if (target === null) {
      await step.run(`${prefix}-2-no-snapshot`, () =>
        runEffect(failRun(run, runId, `${run.volumeName}'s writer on ${A.name} made no snapshot`)));
      return round;
    }
    if (mirrored !== null && mirrored.guid === target.guid) return round;

    const receive = (id: string, token: string | null) =>
      switchStep(id, B, (notAfter) => ({
        command: "start_receive",
        payload: {
          switch: at({ seq: 4, round, sub: 3 })(notAfter),
          name: run.dockerVolume,
          from: ctx.writerAddress,
          base: mirrored?.guid ?? null,
          target: target.name,
          resume_token: token,
        },
      }));
    await receive(`${prefix}-3-receive`, null);
    const received = await pollReceive(step, runEffect, run, runId, B, round, prefix, receive);

    await switchStep(`${prefix}-4-prune`, B, (notAfter) => ({
      command: "prune_mirror",
      payload: mirrorRequest({ seq: 4, round, sub: 4 })(notAfter),
    }));
    await switchStep(`${prefix}-5-commit`, A, (notAfter) => ({
      command: "commit_snapshots",
      payload: { switch: at({ seq: 4, round, sub: 5 })(notAfter), name: run.dockerVolume, mirror_newest: received.guid },
    }));
  }
  return MAX_ROUNDS;
}

async function pollReceive(
  step: StepTools,
  runEffect: EffectRunner,
  run: RunContext,
  runId: string,
  B: MachineRef,
  round: number,
  prefix: string,
  receive: (id: string, token: string | null) => Promise<{ readonly newest: Newest }>,
): Promise<{ readonly guid: SnapshotGuid; readonly name: string }> {
  for (let poll = 0; poll < MAX_POLLS; poll += 1) {
    await step.sleep(`${prefix}-3-wait-${poll}`, POLL_INTERVAL);
    const status = await step.run(`${prefix}-3-inspect-${poll}`, () =>
      runEffect(sendSwitch(run, runId, B, () => ({ command: "inspect_receive", payload: { name: run.dockerVolume, round } }))
        .pipe(Effect.map((view) => view.status))));
    switch (status.state) {
      case "running":
        continue;
      case "done":
        return { guid: status.newest.guid, name: status.newest.name };
      case "resumable":
        await receive(`${prefix}-3-resume-${poll}`, status.token);
        continue;
      case "failed":
        await step.run(`${prefix}-3-failed`, () =>
          runEffect(failRun(run, runId, `${copyName(run.volumeName, B.name)} could not receive ${status.target}: ${status.reason}`)));
        throw new Error("unreachable: failRun ends the run");
      case "idle":
        await step.run(`${prefix}-3-idle`, () =>
          runEffect(failRun(run, runId, `${copyName(run.volumeName, B.name)} stopped receiving without a result`)));
        throw new Error("unreachable: failRun ends the run");
    }
  }
  await step.run(`${prefix}-3-timeout`, () =>
    runEffect(failRun(run, runId, `${copyName(run.volumeName, B.name)} is still receiving after ${MAX_POLLS} checks`)));
  throw new Error("unreachable: failRun ends the run");
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
