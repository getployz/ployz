import { Effect } from "effect";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { createStartupResealEvent, variablesResealRequested } from "#/modules/sealing/sealing.events";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { readClusterSealing } from "#/modules/sealing/cluster-sealing.server";
import type { StoreServices } from "#/modules/config-store/store-deployment.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

const legacyOrganizations = Effect.gen(function* () {
  const store = yield* cloudStore;
  return yield* storeTry(() => store.legacyOrganizations());
});

const legacyCount = Effect.fn("Reseal.count")(function* (organizationId: string) {
  const store = yield* cloudStore;
  return yield* storeTry(() => store.legacyCount(organizationId));
});

const resealEnvironments = Effect.fn("Reseal.environments")(function* (organizationId: string) {
  const store = yield* cloudStore;
  return yield* storeTry(() => store.resealEnvironments(organizationId));
});

const reseal = Effect.fn("Reseal.environment")(function* (environmentId: string, publicKey: string) {
  const store = yield* cloudStore;
  return yield* storeTry(() => store.reseal(environmentId, { publicKey }));
});

/**
 * Reseals `organizationId`'s Legacy secrets to its Cluster's key, one step per Environment so a crash resumes at the
 * next. Without a key it waits: the Cluster's first key report sends this again.
 */
export async function executeResealOrganization(organizationId: string, step: StepTools<"run" | "sendEvent">, runEffect: EffectRunner<StoreServices>) {
  const before = await step.run("count-before", () => runEffect(legacyCount(organizationId)));
  const sealing = await step.run("recipient", () => runEffect(Effect.gen(function* () {
    const sealing = yield* readClusterSealing(organizationId);
    if (sealing.state === "no_server") {
      yield* Effect.logInfo("Reseal awaiting a Server.").pipe(Effect.annotateLogs({ organizationId, ...before }));
    }
    return sealing;
  })));
  if (sealing.state === "no_server") return { awaitingServer: true, ...before };

  const environments = await step.run("environments", () => runEffect(resealEnvironments(organizationId)));
  let resealed = 0;
  for (const environmentId of environments) {
    const done = await step.run(`reseal-${environmentId}`, () => runEffect(reseal(environmentId, sealing.publicKey)));
    resealed += done.resealed;
  }
  const after = await step.run("count-after", () => runEffect(legacyCount(organizationId).pipe(
    Effect.tap(({ current, history }) => Effect.logInfo(`${current} legacy values and ${history} history values`).pipe(
      Effect.annotateLogs({ organizationId, resealed }),
    )),
  )));
  return { resealed, ...after };
}

export async function executeReseal({ event, step }: { event: { data: { organizationId?: string } }; step: StepTools<"run" | "sendEvent"> }, runEffect: EffectRunner<StoreServices>) {
  const { organizationId } = event.data;
  if (organizationId !== undefined) return executeResealOrganization(organizationId, step, runEffect);
  const organizations = await step.run("organizations", () => runEffect(legacyOrganizations));
  if (organizations.length > 0) {
    await step.sendEvent("reseal-organizations", organizations.map((id) => variablesResealRequested.create({ organizationId: id })));
  }
  return { organizations: organizations.length };
}

/** Asks every Organization with Legacy values a reseal converts to reseal, through the outbox so a failed send is retried. */
export const requestResealAtStart = Effect.fn("Reseal.requestAtStart")(function* (startedAt: Date) {
  yield* enqueueEvent(createStartupResealEvent(startedAt));
});

export const createResealVariables = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "reseal-variables",
      retries: 5,
      triggers: [{ event: variablesResealRequested.eventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) => executeReseal({ event, step }, runEffect),
  );
