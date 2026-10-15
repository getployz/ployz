import { Effect } from "effect";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";
import { eachSite } from "../postgres/web-analytics.inngest";
import { rollupSites } from "../postgres/rollup";
import { ClickHouse, type ClickHouseService } from "./client";
import { promoteReads } from "./promote";
import { closeDays, rebuildDirtyDays } from "./rollup";
import { ensureSchema } from "./schema";
import { DRAIN_BATCHES, drainOutbox, forgetDeletedSites } from "./writer";

type EffectRunner = typeof runInngestEffect;

const DRAINS_PER_RUN = 30;

const withClickHouse = <A, E, R>(run: (clickhouse: ClickHouseService) => Effect.Effect<A, E, R>, otherwise: A) =>
  Effect.gen(function* () {
    const clickhouse = yield* ClickHouse;
    return clickhouse === null ? otherwise : yield* run(clickhouse);
  });

const createDrain = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-clickhouse-drain", retries: 3, triggers: [{ cron: "TZ=UTC * * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const configured = await step.run("ensure-schema", () => runEffect(withClickHouse((clickhouse) => ensureSchema(clickhouse).pipe(Effect.as(true)), false)));
      if (!configured) return;
      for (let drain = 0; drain < DRAINS_PER_RUN; drain++) {
        const drained = await step.run(`drain-${drain}`, () => runEffect(withClickHouse(drainOutbox, 0)));
        if (drained < DRAIN_BATCHES) return;
      }
    },
  );

const createRollup = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-clickhouse-rollup", retries: 3, triggers: [{ cron: "TZ=UTC */15 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const now = new Date();
      const sites = await step.run("sites", () => runEffect(withClickHouse(() => rollupSites(), [])));
      await eachSite(sites, async (environmentId) => {
        const closed = await step.run(`close-days-${environmentId}`, () => runEffect(closeDays(environmentId, now)));
        await step.run(`promote-${environmentId}`, () => runEffect(promoteReads(environmentId)));
        return closed;
      });
    },
  );

const createDirtyRebuild = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-clickhouse-dirty", retries: 3, triggers: [{ cron: "TZ=UTC */5 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const sites = await step.run("sites", () => runEffect(withClickHouse(() => rollupSites(), [])));
      await eachSite(sites, (environmentId) => step.run(`rebuild-dirty-${environmentId}`, () => runEffect(rebuildDirtyDays(environmentId))));
    },
  );

const createForget = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-clickhouse-forget", retries: 3, triggers: [{ cron: "TZ=UTC 40 3 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      await step.run("forget-deleted-sites", () => runEffect(withClickHouse(forgetDeletedSites, 0)));
    },
  );

export const createClickHouseFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createDrain(inngest, runEffect),
  createRollup(inngest, runEffect),
  createDirtyRebuild(inngest, runEffect),
  createForget(inngest, runEffect),
];
