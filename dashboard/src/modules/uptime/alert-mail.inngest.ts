import type { Effect } from "effect";
import type { InngestClient, PloyzInngest } from "#/modules/inngest/client";
import { sendUptimeAlerts } from "#/modules/uptime/alert-mail.server";
import type { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import { runInngestEffect } from "#/server/run.server";

type AlertEffectRunner = <A, E extends Error>(program: Effect.Effect<A, E, Database | AppConfig | InngestClient>) => Promise<A>;

/** Each minute, mail the uptime alerts the control plane wrote to uptime_alert. */
export const createSendUptimeAlerts = (inngest: PloyzInngest, runEffect: AlertEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "send-uptime-alerts",
      retries: 3,
      triggers: [{ cron: "* * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("send-pending-alerts", () => runEffect(sendUptimeAlerts(new Date()))),
  );
