import { Schema } from "effect";
import { applyClusterName, foundClusterName, listUnappliedClusterNames } from "#/modules/cluster-name/cluster-name.server";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { clusterNameRequested, createClusterNameRequestedEvent } from "#/modules/cluster-name/cluster-name.events";
import { configFirstServerJoined } from "#/modules/config-store/config-store.events";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

const PATIENCE_SECONDS = 60 * 60;
const delaySeconds = (attempt: number) => Math.min(15 * 2 ** attempt, 120);

export async function executeApplyClusterName(
  { event, step }: { event: { name: string; data: unknown }; step: StepTools<"run" | "sendEvent" | "sleep"> },
  runEffect: EffectRunner,
) {
  const { organizationId } = Schema.decodeUnknownSync(clusterNameRequested.schema)(event.data, { onExcessProperty: "preserve" });
  if (event.name === configFirstServerJoined.name) await step.run("found", () => runEffect(foundClusterName(organizationId)));
  for (let attempt = 0, waited = 0; waited < PATIENCE_SECONDS; attempt += 1) {
    const applied = await step.run(`apply-${attempt}`, () => runEffect(applyClusterName(organizationId)));
    if (applied !== "waiting") return { organizationId, applied };
    waited += delaySeconds(attempt);
    await step.sleep(`wait-${attempt}`, `${delaySeconds(attempt)}s`);
  }
  return { organizationId, applied: "waiting" as const };
}

export async function executeRetryClusterNames({ step }: { step: StepTools<"run" | "sendEvent" | "sleep"> }, runEffect: EffectRunner) {
  const organizationIds = await step.run("list-unapplied", () => runEffect(listUnappliedClusterNames()));
  const hour = new Date().toISOString().slice(0, 13);
  if (organizationIds.length > 0) {
    await step.sendEvent("request-cluster-names", organizationIds.map((organizationId) =>
      createClusterNameRequestedEvent({ id: `cluster-name-retry-${organizationId}-${hour}`, organizationId })));
  }
  return { organizationCount: organizationIds.length };
}

export const createClusterNameFunctions = (inngest: PloyzInngest) => [
  inngest.createFunction(
    {
      id: "apply-cluster-name",
      retries: 3,
      triggers: [{ event: clusterNameRequested.eventType }, { event: configFirstServerJoined.eventType }],
      // One set at a time per Organization, so an older set never lands after a newer one. Each try reads the name saved last.
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) => executeApplyClusterName({ event, step }, runInngestEffect),
  ),
  inngest.createFunction(
    { id: "retry-cluster-names", retries: 3, triggers: [{ cron: "TZ=UTC 30 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => executeRetryClusterNames({ step }, runInngestEffect),
  ),
];
