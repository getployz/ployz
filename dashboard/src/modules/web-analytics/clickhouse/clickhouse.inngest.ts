import { Effect } from "effect";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";
import { ClickHouse, type ClickHouseService } from "./client";
import { ensureSchema } from "./schema";
import { DRAIN_BATCHES, drainOutbox, forgetDeletedSites } from "./writer";

type EffectRunner = typeof runInngestEffect;

const DRAINS_PER_RUN = 30;

/** Without `CLICKHOUSE_URL` ingest writes no outbox, so every ClickHouse job has nothing to do. */
const withClickHouse = <A, E, R>(run: (clickhouse: ClickHouseService) => Effect.Effect<A, E, R>, otherwise: A) =>
  Effect.gen(function* () {
    const clickhouse = yield* ClickHouse;
    return clickhouse === null ? otherwise : yield* run(clickhouse);
  });

const createDrain = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-clickhouse-drain", retries: 3, triggers: [{ cron: "TZ=UTC * * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const configured = await step.run("ensure-schema", () => runEffect(withClickHouse((clickhouse) => ensureSchema(clickhouse).pipe(Effect.as(true)), false)));
      if (!configured) return;
      for (let drain = 0; drain < DRAINS_PER_RUN; drain++) {
        const drained = await step.run(`drain-${drain}`, () => runEffect(withClickHouse(drainOutbox, 0)));
        if (drained < DRAIN_BATCHES) return;
      }
    },
  );

const createForget = (inngest: PloyzInngest, runEffect: EffectRunner) =>
  inngest.createFunction(
    { id: "analytics-clickhouse-forget", retries: 3, triggers: [{ cron: "TZ=UTC 40 3 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      await step.run("forget-deleted-sites", () => runEffect(withClickHouse(forgetDeletedSites, 0)));
    },
  );

export const createClickHouseFunctions = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => [
  createDrain(inngest, runEffect),
  createForget(inngest, runEffect),
];
