import type { Polar } from "#/modules/billing/polar-provider.server";
import type { InngestClient, PloyzInngest } from "#/modules/inngest/client";
import { sendUptimeAlerts } from "#/modules/uptime/alert-mail.server";
import type { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

type AlertEffectRunner = EffectRunner<Database | AppConfig | InngestClient | Polar>;

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
