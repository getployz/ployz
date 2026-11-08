import type { PloyzInngest } from "#/modules/inngest/client";
import { sweepProposals } from "#/modules/proposals/proposals.server";
import { runInngestEffect } from "#/server/run.server";

/** Every minute: an approved plan whose next step is due but never started gets it started. A step starts once. */
export const createSweepProposals = (inngest: PloyzInngest, runEffect: typeof runInngestEffect = runInngestEffect) =>
  inngest.createFunction(
    { id: "sweep-proposals", retries: 1, triggers: [{ cron: "* * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => ({ looked: await step.run("sweep", () => runEffect(sweepProposals(new Date()))) }),
  );
