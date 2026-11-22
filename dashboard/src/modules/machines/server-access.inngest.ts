import type { PloyzInngest } from "#/modules/inngest/client";
import { serverAccessRetireRequested } from "#/modules/machines/machines.events";
import { retireServerAccess } from "#/modules/machines/server-access.server";
import { runInngestEffect } from "#/modules/inngest/run.server";

/**
 * When a member leaves an Organization, and every 5 minutes: revoke device holders whose credential expired or whose
 * user left the Organization, and retry the Clears Servers haven't confirmed.
 */
export const createRetireServerAccess = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "retire-server-access",
      retries: 3,
      triggers: [{ cron: "*/5 * * * *" }, { event: serverAccessRetireRequested.eventType }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("retire-server-access", () => runInngestEffect(retireServerAccess())),
  );
