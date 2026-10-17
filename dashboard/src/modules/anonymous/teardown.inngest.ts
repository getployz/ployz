import { type Effect, Option, Schema } from "effect";
import { listLapsedOrganizationIds, tearDownStep } from "#/modules/anonymous/teardown.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { anonymousTeardownRequestedEventType, createAnonymousTeardownRequestedEvent } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sendEvent">;
type TeardownServices = Effect.Services<ReturnType<typeof tearDownStep>> | Effect.Services<ReturnType<typeof listLapsedOrganizationIds>>;
/** Runs one of the teardown's Effects for an Inngest step. */
export type TeardownEffectRunner = <A, E extends Error>(program: Effect.Effect<A, E, TeardownServices>) => Promise<A>;

const TeardownRequestedData = Schema.Struct({ organizationId: Schema.String.check(Schema.isNonEmpty()) });

/**
 * One step of a lapsed sandbox's teardown, then the next as a fresh event once the Projects are gone. Every run
 * re-derives where the Organization stands, so a retry after a crash does the step the crash left.
 */
export async function executeTearDownAnonymousOrganization(
  { event, step, attempt }: { event: { data: unknown }; step: StepTools; attempt: number },
  runEffect: TeardownEffectRunner,
) {
  const organizationId = await step.run("normalize-organization-id", () => {
    const decoded = Schema.decodeUnknownOption(TeardownRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value.organizationId : null;
  });
  if (organizationId === null) return { organizationId: null, state: null, did: "nothing" as const };
  const result = await step.run("tear-down", () => runEffect(tearDownStep(organizationId, attempt > 0)));
  if (result.did === "removed_projects") {
    await step.sendEvent("request-next-step", [createAnonymousTeardownRequestedEvent({ organizationId })]);
  }
  return result;
}

export async function executeScheduleAnonymousSweep({ step }: { step: StepTools }, runEffect: TeardownEffectRunner) {
  const organizationIds = await step.run("list-lapsed-organizations", () => runEffect(listLapsedOrganizationIds()));
  if (organizationIds.length > 0) {
    await step.sendEvent(
      "request-teardowns",
      organizationIds.map((organizationId) => createAnonymousTeardownRequestedEvent({ organizationId })),
    );
  }
  return { organizationIds };
}

export const createTearDownAnonymousOrganization = (inngest: PloyzInngest, runEffect: TeardownEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "tear-down-anonymous-organization",
      retries: 3,
      triggers: [{ event: anonymousTeardownRequestedEventType }],
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
