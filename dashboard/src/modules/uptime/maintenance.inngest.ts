import type { PloyzInngest } from "#/modules/inngest/client";
import { uptimeBackfillRequested } from "#/modules/uptime/uptime.events";
import { pruneUptimeBatch, rollupUptime } from "#/modules/uptime/maintenance.server";
import { pruneInBatches } from "#/modules/inngest/prune";
import { RETAINED } from "#/modules/uptime/retention";
import { backfillUptimeChecks } from "#/modules/uptime/uptime-sync.server";
import type { Database } from "#/server/database.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

type UptimeEffectRunner = EffectRunner<Database>;

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
      for (const kind of RETAINED) {
        await pruneInBatches(step, `prune-${kind}`, () => runEffect(pruneUptimeBatch(kind, new Date())));
      }
    },
  );

/** On request, once after rollout: give Services applied before uptime checks existed their checks. */
const createBackfillUptime = (inngest: PloyzInngest, runEffect: UptimeEffectRunner) =>
  inngest.createFunction(
    {
      id: "uptime-backfill",
      retries: 3,
      triggers: [{ event: uptimeBackfillRequested.eventType }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("sync-every-environment", () => runEffect(backfillUptimeChecks())),
  );

export const createUptimeMaintenanceFunctions = (inngest: PloyzInngest, runEffect: UptimeEffectRunner = runInngestEffect) => [
  createRollupUptime(inngest, runEffect),
  createPruneUptime(inngest, runEffect),
  createBackfillUptime(inngest, runEffect),
];
