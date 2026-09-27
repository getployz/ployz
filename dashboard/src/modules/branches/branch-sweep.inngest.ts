import { sweepIdleBranches } from "#/modules/branches/branch-close.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";

/** Hourly, the system closes Branches that have gone 7 days without a deploy. */
export const createSweepIdleBranches = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "sweep-idle-branches",
      retries: 3,
      triggers: [{ cron: "0 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("sweep-idle-branches", () => runInngestEffect(sweepIdleBranches(new Date()))),
  );
