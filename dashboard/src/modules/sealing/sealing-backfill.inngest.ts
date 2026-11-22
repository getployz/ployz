import { Option } from "effect";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { eventData } from "#/modules/inngest/define-event";
import { sealingKeyEnsureRequested } from "#/modules/sealing/sealing.events";
import {
  backfillClusterSealingKey,
  handleSealingBackfillReport,
  loadSealingBackfillTarget,
} from "#/modules/sealing/sealing-backfill.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

export async function executeEnsureClusterSealingKey(
  { event, step }: { event: { data: unknown }; step: StepTools<"run"> },
  runEffect: EffectRunner,
) {
  const requested = eventData(sealingKeyEnsureRequested)(event);
  if (Option.isNone(requested)) return { organizationId: null, skipped: true };
  const { organizationId } = requested.value;

  const target = await step.run("load-cluster", () => runEffect(loadSealingBackfillTarget(organizationId)));
  if (target === null) return { organizationId, skipped: true };
  const report = await step.run("backfill", () =>
    runEffect(backfillClusterSealingKey(organizationId, { expected: target.expected, generator: target.generator }, target)));
  if (report === null) return { organizationId, skipped: true };
  const next = await step.run("handle-report", () => runEffect(handleSealingBackfillReport(organizationId, target, report)));
  if (next.next === "stop") return { organizationId, result: next.result, awaitsOperator: next.awaitsOperator };

  const rerun = await step.run("backfill-with-generator", () =>
    runEffect(backfillClusterSealingKey(organizationId, { expected: target.expected, generator: next.generator }, target)));
  if (rerun === null) return { organizationId, skipped: true };
  const after = await step.run("handle-rerun-report", () =>
    runEffect(handleSealingBackfillReport(organizationId, target, rerun)));
  return { organizationId, result: rerun.result, awaitsOperator: after.next === "stop" && after.awaitsOperator };
}

export const createEnsureClusterSealingKey = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "ensure-cluster-sealing-key",
      retries: 3,
      triggers: [{ event: sealingKeyEnsureRequested.eventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
      debounce: { key: "event.data.organizationId", period: "5s", timeout: "30s" },
    },
    async ({ event, step }) => executeEnsureClusterSealingKey({ event, step }, runEffect),
  );
