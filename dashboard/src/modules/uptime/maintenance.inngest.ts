import type { Effect } from "effect";
import type { PloyzInngest } from "#/modules/inngest/client";
import { prunedKinds, pruneUptimeBatch, rollupUptime } from "#/modules/uptime/maintenance.server";
import { PRUNE_BATCH_ROWS } from "#/modules/uptime/retention";
import type { Database } from "#/server/database.server";
import { runInngestEffect } from "#/server/run.server";

type UptimeEffectRunner = <A, E extends Error>(program: Effect.Effect<A, E, Database>) => Promise<A>;

/** 4 kinds at most 200 batches each stays under Inngest's 1,000 steps per run. The rest waits for the next hour. */
const MAX_PRUNE_BATCHES = 200;

/**
 * Hourly at :05, recompute the two closed hours of uptime_result_hourly from raw. The hours follow the run's
 * scheduled time, not the clock, so a run that starts late still covers its own hours.
 */
const createRollupUptime = (inngest: PloyzInngest, runEffect: UptimeEffectRunner) =>
  inngest.createFunction(
    {
      id: "uptime-rollup",
      retries: 3,
      triggers: [{ cron: "5 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ event, step }) =>
      step.run("rollup-closed-hours", () => runEffect(rollupUptime(new Date(event.ts ?? Date.now())))),
  );

/** Hourly, delete uptime history past its retention, one batch per step. */
const createPruneUptime = (inngest: PloyzInngest, runEffect: UptimeEffectRunner) =>
  inngest.createFunction(
    {
      id: "uptime-prune",
      retries: 3,
      triggers: [{ cron: "30 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => {
      for (const kind of prunedKinds) {
        for (let batch = 0; batch < MAX_PRUNE_BATCHES; batch += 1) {
          const deleted = await step.run(`prune-${kind}`, () => runEffect(pruneUptimeBatch(kind, new Date())));
          if (deleted < PRUNE_BATCH_ROWS) break;
        }
      }
    },
  );

export const createUptimeMaintenanceFunctions = (inngest: PloyzInngest, runEffect: UptimeEffectRunner = runInngestEffect) => [
  createRollupUptime(inngest, runEffect),
  createPruneUptime(inngest, runEffect),
];
