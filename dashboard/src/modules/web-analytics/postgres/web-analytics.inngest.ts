import { NonRetriableError } from "inngest";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";
import { dropPartition } from "./partitions";
import { closeDays, deleteExpiredAggregates, ensureComingPartitions, expiredPartitions, rebuildDirtyDays, rollupSites } from "./rollup";

type EffectRunner = typeof runInngestEffect;

/**
 * Runs one step per site in order. A site's step retries on its own; one that still fails is collected rather than
 * thrown, so the sites after it run, and the run fails at the end naming every failed site.
 */
const eachSite = async (sites: ReadonlyArray<string>, run: (environmentId: string) => Promise<number>) => {
  const failed: Array<{ environmentId: string; error: unknown }> = [];
  for (const environmentId of sites) {
    try {
      await run(environmentId);
    } catch (error) {
      failed.push({ environmentId, error });
    }
  }
  if (failed.length > 0) {
    throw new NonRetriableError(`analytics steps failed for ${failed.map((site) => site.environmentId).join(", ")}`, { cause: failed });
  }
};

const createRollup = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-rollup", retries: 3, triggers: [{ cron: "TZ=UTC */15 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const now = new Date();
      const sites = await step.run("sites", () => runEffect(rollupSites()));
      await eachSite(sites, (environmentId) => step.run(`close-days-${environmentId}`, () => runEffect(closeDays(environmentId, now))));
    },
  );

const createDirtyRebuild = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-dirty", retries: 3, triggers: [{ cron: "TZ=UTC */5 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const sites = await step.run("sites", () => runEffect(rollupSites()));
      await eachSite(sites, (environmentId) => step.run(`rebuild-dirty-${environmentId}`, () => runEffect(rebuildDirtyDays(environmentId))));
    },
  );

const createRetention = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-retention", retries: 3, triggers: [{ cron: "TZ=UTC 20 3 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const now = new Date();
      await step.run("ensure-partitions", () => runEffect(ensureComingPartitions(now)));
      const months = await step.run("expired-partitions", () => runEffect(expiredPartitions(now)));
      for (const month of months) {
        await step.run(`drop-${month}`, () => runEffect(dropPartition(month)));
      }
      await step.run("delete-aggregates", () => runEffect(deleteExpiredAggregates(now)));
    },
  );

export const createWebAnalyticsFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createRollup(inngest, runEffect),
  createDirtyRebuild(inngest, runEffect),
  createRetention(inngest, runEffect),
];
