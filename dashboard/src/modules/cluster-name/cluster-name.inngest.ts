import { Schema } from "effect";
import { applyClusterName, foundClusterName, listUnappliedClusterNames } from "#/modules/cluster-name/cluster-name.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import {
  clusterNameRequestedEventType,
  configFirstServerJoinedEvent,
  configFirstServerJoinedEventType,
  createClusterNameRequestedEvent,
} from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sendEvent" | "sleep">;
type EffectRunner = typeof runInngestEffect;

const Requested = Schema.Struct({ organizationId: Schema.String.check(Schema.isNonEmpty()) });

const PATIENCE_SECONDS = 60 * 60;
const delaySeconds = (attempt: number) => Math.min(15 * 2 ** attempt, 120);

export async function executeApplyClusterName(
  { event, step }: { event: { name: string; data: unknown }; step: StepTools },
  runEffect: EffectRunner,
) {
  const { organizationId } = Schema.decodeUnknownSync(Requested)(event.data, { onExcessProperty: "preserve" });
  if (event.name === configFirstServerJoinedEvent) await step.run("found", () => runEffect(foundClusterName(organizationId)));
  for (let attempt = 0, waited = 0; waited < PATIENCE_SECONDS; attempt += 1) {
    const applied = await step.run(`apply-${attempt}`, () => runEffect(applyClusterName(organizationId)));
    if (applied !== "waiting") return { organizationId, applied };
    waited += delaySeconds(attempt);
    await step.sleep(`wait-${attempt}`, `${delaySeconds(attempt)}s`);
  }
  return { organizationId, applied: "waiting" as const };
}

export async function executeRetryClusterNames({ step }: { step: StepTools }, runEffect: EffectRunner) {
  const organizationIds = await step.run("list-unapplied", () => runEffect(listUnappliedClusterNames()));
  const hour = new Date().toISOString().slice(0, 13);
  if (organizationIds.length > 0) {
    await step.sendEvent("request-cluster-names", organizationIds.map((organizationId) =>
      createClusterNameRequestedEvent({ id: `cluster-name-retry-${organizationId}-${hour}`, organizationId })));
  }
  return { organizationCount: organizationIds.length };
}

export const createClusterNameFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  inngest.createFunction(
    {
      id: "apply-cluster-name",
      retries: 3,
      triggers: [{ event: clusterNameRequestedEventType }, { event: configFirstServerJoinedEventType }],
      // The newest request replaces a run sleeping between tries: each try reads the name saved last.
      singleton: { key: "event.data.organizationId", mode: "cancel" },
    },
    async ({ event, step }) => executeApplyClusterName({ event, step }, runEffect),
  ),
  inngest.createFunction(
    { id: "retry-cluster-names", retries: 3, triggers: [{ cron: "TZ=UTC 30 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => executeRetryClusterNames({ step }, runEffect),
  ),
];
