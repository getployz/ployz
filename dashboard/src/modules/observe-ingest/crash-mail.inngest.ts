import { Effect } from "effect";
import type { PloyzInngest } from "#/modules/inngest/client";
import { observeCrashReceivedEventType } from "#/modules/inngest/events";
import { claimCrashMail, PRUNE_BATCH_ROWS, pruneCrashEvents } from "#/modules/observe-ingest/crash-mail.server";
import { observeServerNames } from "#/modules/server-upgrade/server-upgrade.server";
import { runInngestEffect } from "#/server/run.server";

type EffectRunner = typeof runInngestEffect;

const MAX_CLAIMS = 3;
const MAX_PRUNE_BATCHES = 200;
const SERVICE_KEY = "event.data.organizationId + '/' + event.data.serviceId";

const createMailCrashes = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    {
      id: "mail-crashes",
      retries: 3,
      triggers: [{ event: observeCrashReceivedEventType }],
      debounce: { key: SERVICE_KEY, period: "10s", timeout: "30s" },
      // One run per Service. A newer crash replaces a run sleeping out the mail gap, and the replacement starts after that
      // crash's row committed, so its claim sees it. Skip would drop a crash that lands after a run's last claim.
      singleton: { key: SERVICE_KEY, mode: "cancel" },
      concurrency: [{ key: SERVICE_KEY, limit: 1 }],
    },
    async ({ event, step }) => {
      const { organizationId, serviceId } = event.data;
      const names = await step.run("server-names", () =>
        runEffect(observeServerNames(organizationId).pipe(Effect.map((map) => [...map]))));
      for (let claim = 0; claim < MAX_CLAIMS; claim += 1) {
        const claimed = await step.run(`claim-${claim}`, () =>
          runEffect(claimCrashMail(organizationId, serviceId, new Date(), new Map(names))));
        if (claimed.kind !== "wait") return claimed;
        await step.sleepUntil(`mail-gap-${claim}`, claimed.until);
      }
      return { kind: "none" as const };
    },
  );

const createPruneCrashEvents = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    {
      id: "prune-crash-events",
      retries: 3,
      triggers: [{ cron: "TZ=UTC 40 3 * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => {
      for (let batch = 0; batch < MAX_PRUNE_BATCHES; batch += 1) {
        const deleted = await step.run(`prune-${batch}`, () => runEffect(pruneCrashEvents(new Date())));
        if (deleted < PRUNE_BATCH_ROWS) break;
      }
    },
  );

export const createCrashMailFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createMailCrashes(inngest, runEffect),
  createPruneCrashEvents(inngest, runEffect),
];
