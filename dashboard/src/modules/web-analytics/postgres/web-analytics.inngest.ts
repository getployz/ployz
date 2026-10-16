import type { PloyzInngest } from "#/modules/inngest/client";
import { pruneSalts } from "#/modules/web-analytics/salts.server";
import { runInngestEffect } from "#/server/run.server";
import { dropPartition, ensureComingPartitions, expiredPartitions } from "./partitions";

type EffectRunner = typeof runInngestEffect;

const createRetention = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-retention", retries: 3, triggers: [{ cron: "TZ=UTC 20 3 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const now = new Date();
      await step.run("ensure-partitions", () => runEffect(ensureComingPartitions(now)));
      const days = await step.run("expired-partitions", () => runEffect(expiredPartitions(now)));
      for (const day of days) {
        await step.run(`drop-${day}`, () => runEffect(dropPartition(day)));
      }
      await step.run("prune-salts", () => runEffect(pruneSalts(now)));
    },
  );

export const createWebAnalyticsFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createRetention(inngest, runEffect),
];
