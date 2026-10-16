import { Option, Schema } from "effect";
import type { GroupTools } from "inngest";
import type { MachineId } from "#/db/tables";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import {
  createOrganizationFoundedEvent,
  organizationFoundedEvent,
  serverEnrollmentCancelEvent,
  serverEnrollmentCompletedEvent,
  serverEnrollmentReportedEvent,
  serverEnrollmentRequestedEventType,
} from "#/modules/inngest/events";
import {
  asStage,
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

/** Joiners wait this long for the founder: 12 rounds of 10 m. */
export const FOUNDER_ROUNDS = 12;
const FOUNDER_WAIT = "10m";
/** Quiet rounds before the attempt gives up: 12 of 30 m. */
export const QUIET_ROUNDS = 12;
const QUIET_WAIT = "30m";
/** A bound on reported stages, so a host that loops its stages can't run the step budget out. */
const MAX_EVENTS = 100;

const Requested = Schema.Struct({
  attemptId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
});

const Reported = Schema.Struct({
  name: Schema.Literal(serverEnrollmentReportedEvent),
  data: Schema.Struct({
    stage: Schema.NullOr(Schema.String),
    failure: Schema.optionalKey(Schema.Struct({ stage: Schema.NullOr(Schema.String), message: Schema.String })),
  }),
});
const Completed = Schema.Struct({ name: Schema.Literal(serverEnrollmentCompletedEvent) });
const Heard = Schema.Union([Reported, Completed]);
const decodeHeard = Schema.decodeUnknownOption(Heard);

type Ctx = { event: { data: unknown }; step: StepTools; runId: string; group: Pick<GroupTools, "parallel"> };

/**
 * One Server Enrollment: bind the run → a joiner waits for its Organization's founder → follow the host's check-ins
 * until its Machine completes, it reports a failure, or it goes quiet. Check-in answers the host from the row; this run
 * is the row's only writer after it is planned.
 */
export async function executeServerEnrollment({ event, step, runId, group }: Ctx, runEffect: EffectRunner) {
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
  const reportedFor = (id: string, timeout: string) =>
    step.waitForEvent(id, { event: serverEnrollmentReportedEvent, if: `async.data.attemptId == '${attemptId}'`, timeout });

  if (bound.state === "awaiting_founder") {
    let released = false;
    for (let round = 0; round < FOUNDER_ROUNDS && !released; round += 1) {
      if (await step.run(`founded-${round}`, () => runEffect(organizationFounded(organizationId)))) {
        released = await step.run("release", () => runEffect(releaseJoiner(attemptId, runId)));
        if (!released) return { attemptId, state: "settled" as const };
        break;
      }
      const heard = await group.parallel({ mode: "race" }, () => Promise.race([
        step.waitForEvent(`founder-wait-${round}`, {
          event: organizationFoundedEvent, if: `async.data.organizationId == '${organizationId}'`, timeout: FOUNDER_WAIT,
        }),
        reportedFor(`founder-report-${round}`, FOUNDER_WAIT),
      ]));
      const failure = heard === null ? undefined : Option.getOrUndefined(decodeHeard(heard));
      if (failure?.name === serverEnrollmentReportedEvent && failure.data.failure !== undefined) {
        return fail(`fail-founder-${round}`, failure.data.failure.stage, failure.data.failure.message);
      }
    }
    if (!released) return fail("fail-no-founder", null, "First server didn't finish");
  }

  let quiet = 0;
  for (let heardCount = 0; heardCount < MAX_EVENTS && quiet < QUIET_ROUNDS; heardCount += 1) {
    const heard = await group.parallel({ mode: "race" }, () => Promise.race([
      reportedFor(`report-${heardCount}`, QUIET_WAIT),
      step.waitForEvent(`complete-${heardCount}`, {
        event: serverEnrollmentCompletedEvent, if: `async.data.attemptId == '${attemptId}'`, timeout: QUIET_WAIT,
      }),
    ]));
    const decoded = heard === null ? undefined : Option.getOrUndefined(decodeHeard(heard));
    if (decoded === undefined || decoded.name === serverEnrollmentCompletedEvent) {
      if (decoded === undefined) quiet += 1;
      // The token records the Machine that joined, so a completion is read from it rather than from the event.
      const found = await step.run(`check-${heardCount}`, () => runEffect(checkServerEnrollment(attemptId)));
      if (found.kind === "gone") return { attemptId, state: "settled" as const };
      if (found.kind === "joined") return join(`join-${heardCount}`, found.machineId);
      if (found.kind === "silent") return fail(`fail-silent-${heardCount}`, null, "Server stopped checking in");
      continue;
    }
    const { failure } = decoded.data;
    if (failure !== undefined) return fail(`fail-${heardCount}`, failure.stage, failure.message);
    await step.run(`stage-${heardCount}`, () => runEffect(recordStage(attemptId, runId, decoded.data.stage)));
  }
  return fail("fail-timeout", null, "Enrollment timed out");

  async function join(id: string, machineId: MachineId) {
    const joined = await step.run(id, () => runEffect(endServerEnrollment(attemptId, runId, { state: "joined", machineId })));
    if (joined && bound.kind === "bound" && bound.role === "founder") {
      await step.sendEvent("send-founded", createOrganizationFoundedEvent(attemptId, { organizationId }));
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
  closeRun: (runId, end, triggering) =>
    closeServerEnrollment(runId, end, Option.isSome(triggering) ? triggering.value.data.attemptId : undefined),
  sweep: { id: "server-enrollment-sweep", closeStale: closeStaleServerEnrollments },
});

export const createServerEnrollmentFunctions = lifecycle.createFunctions;
