import { Option, Schema } from "effect";
import type { MachineId } from "#/db/tables";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import {
  createServerEnrollmentFoundedEvent,
  serverEnrollmentCancelEvent,
  serverEnrollmentReportedEvent,
  serverEnrollmentRequestedEventType,
} from "#/modules/inngest/events";
import {
  asStage,
  awaitingJoiners,
  bindServerEnrollment,
  checkServerEnrollment,
  closeServerEnrollment,
  closeStaleServerEnrollments,
  endServerEnrollment,
  organizationFounded,
  recordStage,
  releaseJoiner,
} from "#/modules/machines/waiting/enrollment-run.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "waitForEvent" | "sendEvent">;
type EffectRunner = typeof runInngestEffect;

export const FOUNDER_ROUNDS = 12;
const FOUNDER_WAIT = "10m";
export const QUIET_ROUNDS = 12;
const QUIET_WAIT = "30m";
/** A bound on reported stages, so a host that loops its stages can't run the step budget out. */
const MAX_EVENTS = 100;

const Requested = Schema.Struct({
  attemptId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
});

const Heard = Schema.Struct({
  name: Schema.Literal(serverEnrollmentReportedEvent),
  data: Schema.Union([
    Schema.Struct({
      stage: Schema.NullOr(Schema.String),
      failure: Schema.optionalKey(Schema.Struct({ stage: Schema.NullOr(Schema.String), message: Schema.String })),
    }),
    Schema.Struct({ completed: Schema.Literal(true) }),
    Schema.Struct({ founded: Schema.Literal(true) }),
  ]),
});
const decodeHeard = Schema.decodeUnknownOption(Heard);

type Ctx = { event: { data: unknown }; step: StepTools; runId: string };

export async function executeServerEnrollment({ event, step, runId }: Ctx, runEffect: EffectRunner) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(Requested)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const { attemptId, organizationId } = request;
  const bound = await step.run("bind-run", () => runEffect(bindServerEnrollment(request, runId)));
  if (bound.kind === "settled") return { attemptId, state: bound.state };

  const fail = (id: string, stage: string | null, message: string) =>
    step.run(id, () => runEffect(endServerEnrollment(attemptId, runId, { state: "failed", failure: { stage: asStage(stage), message } })))
      .then(() => ({ attemptId, state: "failed" as const, message }));
  const heardFor = async (id: string, timeout: string) => {
    const heard = await step.waitForEvent(id, { event: serverEnrollmentReportedEvent, if: `async.data.attemptId == '${attemptId}'`, timeout });
    return heard === null ? undefined : Option.getOrUndefined(decodeHeard(heard))?.data;
  };

  if (bound.state === "awaiting_founder") {
    let released = false;
    for (let round = 0; round <= FOUNDER_ROUNDS && !released; round += 1) {
      if (await step.run(`founded-${round}`, () => runEffect(organizationFounded(organizationId)))) {
        released = await step.run("release", () => runEffect(releaseJoiner(attemptId, runId)));
        if (!released) return { attemptId, state: "settled" as const };
        break;
      }
      if (round === FOUNDER_ROUNDS) break;
      const heard = await heardFor(`founder-wait-${round}`, FOUNDER_WAIT);
      if (heard !== undefined && "failure" in heard && heard.failure !== undefined) {
        return fail(`fail-founder-${round}`, heard.failure.stage, heard.failure.message);
      }
    }
    if (!released) return fail("fail-no-founder", null, "First server didn't finish");
  }

  let quiet = 0;
  for (let heardCount = 0; heardCount < MAX_EVENTS && quiet < QUIET_ROUNDS; heardCount += 1) {
    const heard = await heardFor(`report-${heardCount}`, QUIET_WAIT);
    if (heard !== undefined && "founded" in heard) continue;
    if (heard === undefined || "completed" in heard) {
      if (heard === undefined) quiet += 1;
      const found = await step.run(`check-${heardCount}`, () => runEffect(checkServerEnrollment(attemptId)));
      if (found.kind === "gone") return { attemptId, state: "settled" as const };
      if (found.kind === "removed") return fail(`fail-removed-${heardCount}`, null, "Enrollment token removed");
      if (found.kind === "joined") return join(`join-${heardCount}`, found.machineId);
      if (found.kind === "silent") return fail(`fail-silent-${heardCount}`, null, "Server stopped checking in");
      continue;
    }
    if (heard.failure !== undefined) return fail(`fail-${heardCount}`, heard.failure.stage, heard.failure.message);
    await step.run(`stage-${heardCount}`, () => runEffect(recordStage(attemptId, runId, heard.stage)));
  }
  return fail("fail-timeout", null, "Enrollment timed out");

  async function join(id: string, machineId: MachineId) {
    const joined = await step.run(id, () => runEffect(endServerEnrollment(attemptId, runId, { state: "joined", machineId })));
    if (joined && bound.kind === "bound" && bound.role === "founder") {
      const joiners = await step.run("awaiting-joiners", () => runEffect(awaitingJoiners(organizationId)));
      if (joiners.length > 0) {
        await step.sendEvent("send-founded", joiners.map((joiner) => createServerEnrollmentFoundedEvent(attemptId, joiner)));
      }
    }
    return { attemptId, state: "joined" as const, machineId };
  }
}

const lifecycle = attemptLifecycle({
  run: {
    id: "server-enrollment",
    triggers: [{ event: serverEnrollmentRequestedEventType }],
    cancelOn: [{ event: serverEnrollmentCancelEvent, match: "data.attemptId" }],
    handler: executeServerEnrollment,
  },
  cancelId: "cancel-server-enrollment",
  triggeringEvent: Schema.Struct({ data: Schema.Struct({ attemptId: Schema.String }) }),
  triggeringOnFailure: true,
  closeRun: (runId, end, triggering) =>
    closeServerEnrollment(runId, end, Option.isSome(triggering) ? triggering.value.data.attemptId : undefined),
  sweep: { id: "server-enrollment-sweep", closeStale: closeStaleServerEnrollments },
});

export const createServerEnrollmentRun = lifecycle.createRun;
export const createServerEnrollmentFunctions = lifecycle.createFunctions;
