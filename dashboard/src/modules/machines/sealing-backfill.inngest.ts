import { Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { sealingKeyEnsureRequestedEventType } from "#/modules/inngest/events";
import {
  backfillClusterSealingKey,
  handleSealingBackfillReport,
  loadSealingBackfillTarget,
} from "#/modules/machines/sealing-backfill.server";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run">;
type EffectRunner = typeof runInngestEffect;

const EnsureRequestedData = Schema.Struct({ organizationId: Schema.String.check(Schema.isNonEmpty()) });

export async function executeEnsureClusterSealingKey(
  { event, step }: { event: { data: unknown }; step: StepTools },
  runEffect: EffectRunner,
) {
  const organizationId = await step.run("normalize-organization-id", () => {
    const decoded = Schema.decodeUnknownOption(EnsureRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value.organizationId : null;
  });
  if (organizationId === null) return { organizationId: null, skipped: true };

  const target = await step.run("load-cluster", () => runEffect(loadSealingBackfillTarget(organizationId)));
  if (target === null) return { organizationId, skipped: true };
  const report = await step.run("backfill", () =>
    runEffect(backfillClusterSealingKey(organizationId, { expected: target.expected, generator: target.generator })));
  if (report === null) return { organizationId, skipped: true };
  const next = await step.run("handle-report", () => runEffect(handleSealingBackfillReport(organizationId, target.clusterKey, report)));
  if (next.next === "stop") return { organizationId, result: next.result, awaitsOperator: next.awaitsOperator };

  const rerun = await step.run("backfill-with-generator", () =>
    runEffect(backfillClusterSealingKey(organizationId, { expected: target.expected, generator: next.generator })));
  if (rerun === null) return { organizationId, skipped: true };
  const after = await step.run("handle-rerun-report", () =>
    runEffect(handleSealingBackfillReport(organizationId, target.clusterKey, rerun)));
  return { organizationId, result: rerun.result, awaitsOperator: after.next === "stop" && after.awaitsOperator };
}

export const createEnsureClusterSealingKey = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "ensure-cluster-sealing-key",
      retries: 3,
      triggers: [{ event: sealingKeyEnsureRequestedEventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) => executeEnsureClusterSealingKey({ event, step }, runEffect),
  );
