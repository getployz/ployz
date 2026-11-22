import { type Effect, Option } from "effect";
import { listLapsedOrganizationIds, tearDownStep } from "#/modules/sandbox/teardown.server";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { anonymousTeardownRequested } from "#/modules/sandbox/anonymous.events";
import { eventData } from "#/modules/inngest/define-event";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

type TeardownServices = Effect.Services<ReturnType<typeof tearDownStep>> | Effect.Services<ReturnType<typeof listLapsedOrganizationIds>>;
/** Runs one of the teardown's Effects for an Inngest step. */
export type TeardownEffectRunner = EffectRunner<TeardownServices>;

/**
 * One step of a lapsed sandbox's teardown, then the next as a fresh event once the Projects are gone. Every run
 * re-derives where the Organization stands, so a retry after a crash does the step the crash left. A retry never forgets a
 * Server: the failed attempt may have heard an answer Cloud couldn't record, so the next sweep decides.
 */
export async function executeTearDownAnonymousOrganization(
  { event, step, attempt }: { event: { data: unknown }; step: StepTools<"run" | "sendEvent">; attempt: number },
  runEffect: TeardownEffectRunner,
) {
  const requested = eventData(anonymousTeardownRequested)(event);
  if (Option.isNone(requested)) return { organizationId: null, state: null, did: "nothing" as const };
  const { organizationId } = requested.value;
  const result = await step.run("tear-down", () => runEffect(tearDownStep(organizationId, attempt === 0)));
  if (result.did === "removed_projects") {
    await step.sendEvent("request-next-step", [anonymousTeardownRequested.create({ organizationId })]);
  }
  return result;
}

export async function executeScheduleAnonymousSweep({ step }: { step: StepTools<"run" | "sendEvent"> }, runEffect: TeardownEffectRunner) {
  const organizationIds = await step.run("list-lapsed-organizations", () => runEffect(listLapsedOrganizationIds()));
  if (organizationIds.length > 0) {
    await step.sendEvent(
      "request-teardowns",
      organizationIds.map((organizationId) => anonymousTeardownRequested.create({ organizationId })),
    );
  }
  return { organizationIds };
}

export const createTearDownAnonymousOrganization = (inngest: PloyzInngest, runEffect: TeardownEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "tear-down-anonymous-organization",
      retries: 3,
      triggers: [{ event: anonymousTeardownRequested.eventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step, attempt }) => executeTearDownAnonymousOrganization({ event, step, attempt }, runEffect),
  );

export const createScheduleAnonymousSweep = (inngest: PloyzInngest, runEffect: TeardownEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "schedule-anonymous-sweep",
      retries: 3,
      triggers: [{ cron: "TZ=UTC */15 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => executeScheduleAnonymousSweep({ step }, runEffect),
  );
