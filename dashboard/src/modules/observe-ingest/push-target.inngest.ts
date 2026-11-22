import type { PloyzInngest } from "#/modules/inngest/client";
import { observePushTargetRequested } from "#/modules/observe-ingest/observe.events";
import { deliverPushTargets } from "#/modules/observe-ingest/push-target.server";
import { runInngestEffect } from "#/modules/inngest/run.server";

/** After each capability publish, and every 5 minutes: point Machines that haven't confirmed it at Cloud's observe URL. */
export const createDeliverPushTargets = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "deliver-observe-push-targets",
      retries: 3,
      triggers: [{ cron: "*/5 * * * *" }, { event: observePushTargetRequested.eventType }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("deliver-push-targets", () => runInngestEffect(deliverPushTargets())),
  );
