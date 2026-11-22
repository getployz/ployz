import { pruneChangeLog } from "#/collections/change-log/change-log.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/modules/inngest/run.server";

/** The Organization change log keeps 24 hours; older cursors fall back to a full read. */
export const createPruneOrganizationChangeLog = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "prune-organization-change-log",
      retries: 3,
      triggers: [{ cron: "0 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("prune-organization-change-log", () => runInngestEffect(pruneChangeLog())),
  );
