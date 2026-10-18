import { sweepExpiredVerifications } from "#/modules/identity/verification-sweep.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";

export const createSweepExpiredVerifications = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "sweep-expired-verifications",
      retries: 3,
      triggers: [{ cron: "0 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("sweep-expired-verifications", () => runInngestEffect(sweepExpiredVerifications())),
  );
