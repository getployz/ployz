import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";
import { applyRetention, rebuildDirtyDays, rollupClosedDays } from "./rollup";

type EffectRunner = typeof runInngestEffect;

/** Every 15 minutes, close every site-local day that ended a session gap ago. The jobs read the clock, not the event,
 * because a day closes by wall time and a late run should close what is closed now. */
const createRollup = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-rollup", retries: 3, triggers: [{ cron: "TZ=UTC */15 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => step.run("close-days", () => runEffect(rollupClosedDays(new Date()))),
  );

/** Every 5 minutes, rebuild the closed days a late pageview landed in. */
const createDirtyRebuild = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-dirty", retries: 3, triggers: [{ cron: "TZ=UTC */5 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => step.run("rebuild-dirty-days", () => runEffect(rebuildDirtyDays())),
  );

/** Daily, create the coming months' partitions and drop what is past the longest retention. */
const createRetention = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-retention", retries: 3, triggers: [{ cron: "TZ=UTC 20 3 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => step.run("apply-retention", () => runEffect(applyRetention(new Date()))),
  );

export const createWebAnalyticsFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createRollup(inngest, runEffect),
  createDirtyRebuild(inngest, runEffect),
  createRetention(inngest, runEffect),
];
