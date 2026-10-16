import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { ClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { backfillSite, drainOutbox, DRAIN_BATCHES, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const BUSY = "00000000-0000-4000-8000-00000000fd10";
const QUIET = "00000000-0000-4000-8000-00000000fd11";
const SMALLER = 3;

it.live("one drain takes each site's numerically oldest batches, so 301 queued batches leave exactly 201..301", () => Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(op: Effect.Effect<A, E, R>) => op.pipe(Effect.provideService(ClickHouse, clickhouse), Effect.provide(services), Effect.orDie);
  for (const site of [BUSY, QUIET]) {
    yield* provide(seedSite({ environmentId: site, timezone: "UTC" }, []));
    yield* provide(backfillSite(clickhouse, site));
  }
  const ts = new Date(Math.floor(Date.now() / 86_400_000) * 86_400_000 - 86_400_000 + 36_000_000).toISOString();
  yield* provide(ensurePartitions([monthOf(new Date(ts))]));
  const enqueue = (site: string, visitorId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const row: PageviewRecord = {
      ts, visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
      utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
    };
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(site, [row])).returning());
  }));
  for (let i = 1; i <= 301; i++) {
    yield* enqueue(BUSY, String(i));
    if (i <= SMALLER) yield* enqueue(QUIET, String(i));
  }
  const database = yield* provide(Database);
  const queued = (site: string) => provide(database.drizzle.execute<{ id: number }>(sql`
    select id::int as id from analytics.outbox where environment_id = ${site} order by analytics.outbox.id`, "objects"))
    .pipe(Effect.map((rows) => rows.map((row) => row.id)));
  const before = yield* queued(BUSY);
  expect(before).toHaveLength(301);
  expect(yield* provide(drainOutbox(clickhouse))).toBe(DRAIN_BATCHES + SMALLER);
  expect(yield* queued(QUIET)).toEqual([]);
  expect(yield* queued(BUSY)).toEqual(before.slice(DRAIN_BATCHES));
}), 60_000);
