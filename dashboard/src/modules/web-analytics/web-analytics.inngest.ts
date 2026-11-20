import type { PloyzInngest } from "#/modules/inngest/client";
import { pruneSalts } from "#/modules/web-analytics/salts.server";
import { runInngestEffect } from "#/server/run.server";

type EffectRunner = typeof runInngestEffect;

/** Requests expire in ClickHouse by their own retention days; what Cloud keeps is each site's day salts, dropped once their day is out of retention. */
const createRetention = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-retention", retries: 3, triggers: [{ cron: "TZ=UTC 20 3 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      await step.run("prune-salts", () => runEffect(pruneSalts(new Date())));
    },
  );

export const createWebAnalyticsFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createRetention(inngest, runEffect),
];
