import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Deferred, Effect, Exit, Fiber, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, pin, read } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService, type Params } from "#/modules/web-analytics/clickhouse/client";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const HOUR = 3_600_000;
const SITE = "00000000-0000-4000-8000-00000000c6a4";
const LIFETIME_SECONDS = 4;

const setup = Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse: real } = yield* clickhouseTestDatabase;
  const clickhouse: ClickHouseService = { ...real, writeLifetimeSeconds: LIFETIME_SECONDS };
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
    effect.pipe(Effect.provideService(ClickHouse, clickhouse), Effect.provide(services), Effect.orDie);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const row = (visitorId: string): PageviewRecord => ({
    ts: new Date(today - 3 * DAY + 10 * HOUR).toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const query = { environmentId: SITE, timezone: "UTC", range: { start: new Date(today - 3 * DAY), end: new Date(today - 2 * DAY) }, filters: [] };
  const daily = provide(Effect.map(store.dailyTotals(query), (rows) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0)));
  const enqueue = (visitorId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(SITE, [row(visitorId)])).returning());
  }));
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, [row("1"), row("2")]));
  yield* provide(backfillSite(clickhouse, SITE));
  const pendingCount = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ n: number }>(sql`
      select cardinality(pending) as n from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
    return row?.n ?? 0;
  }));
  return { clickhouse, provide, daily, enqueue, pendingCount, query };
});

it.live("a retried GC keeps an interrupted INSERT excluded until its write lifetime has passed", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, enqueue, pendingCount } = yield* setup;
  yield* enqueue("3");
  const database = yield* provide(Database);
  const minted = yield* Deferred.make<void>();
  const resume = yield* Deferred.make<void>();
  let statements = 0;
  const { drizzle } = database;
  const gated: typeof drizzle = Object.assign(Object.create(drizzle) as typeof drizzle, {
    execute: (...args: Parameters<typeof drizzle.execute>) => Effect.gen(function* () {
      if (++statements === 2) {
        yield* Deferred.succeed(minted, undefined);
        yield* Deferred.await(resume);
      }
      return yield* drizzle.execute(...args);
    }),
  });
  yield* clickhouse.unchecked.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");
  const writing = yield* Effect.forkChild(provide(drainOutbox(clickhouse).pipe(
    Effect.provideService(Database, { ...database, drizzle: gated }))));
  yield* Deferred.await(minted);
  const fence = yield* provide(pin());
  yield* Deferred.succeed(resume, undefined);
  let running = false;
  for (let i = 0; i < 200 && !running; i++) {
    const [row] = yield* clickhouse.unchecked.select<{ n: number }>("SELECT count() AS n FROM system.processes WHERE query LIKE 'INSERT INTO%pageview%'");
    running = Number(row?.n ?? 0) > 0;
    yield* Effect.sleep("5 millis");
  }
  expect(running).toBe(true);
  yield* Fiber.interrupt(writing);
  const failing: ClickHouseService = {
    ...clickhouse,
    unchecked: { ...clickhouse.unchecked, select: () => Effect.fail(new ClickHouseFailure({ message: "GC count failed after committing fence", cause: null })) },
  };
  yield* provide(Effect.flip(collectGarbage(failing, fence)));
  expect(yield* pendingCount).toBe(1);
  yield* provide(collectGarbage(clickhouse, fence));
  expect(yield* pendingCount).toBe(1);
  yield* Effect.sleep(`${LIFETIME_SECONDS * 1000 + 500} millis`);
  yield* provide(collectGarbage(clickhouse, fence));
  expect(yield* pendingCount).toBe(0);
  yield* provide(drainOutbox(clickhouse));
  expect(yield* daily).toBe(3);
  yield* provide(backfillSite(clickhouse, SITE));
  expect(yield* daily).toBe(3);
}), 120_000);

it.live("a Reader query that resolves after its callback closed dies instead of answering", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, query } = yield* setup;
  const started = yield* Deferred.make<void>();
  const resume = yield* Deferred.make<void>();
  const stalled: ClickHouseService = {
    ...clickhouse,
    unchecked: {
      ...clickhouse.unchecked,
      select: <A>(text: string, params?: Params) => Deferred.succeed(started, undefined).pipe(
        Effect.andThen(Deferred.await(resume)), Effect.andThen(clickhouse.unchecked.select<A>(text, params))),
    },
  };
  const escaped = yield* provide(read(stalled, SITE, (reader) => Effect.gen(function* () {
    const b = store.binder(query, reader.snapshot);
    const fiber = yield* Effect.forkDetach(reader.select<{ n: number }>(`SELECT count() AS n FROM pageview WHERE environment_id = ${b.env} AND ${b.visible}`, b.params));
    yield* Deferred.await(started);
    return fiber;
  })));
  yield* provide(backfillSite(clickhouse, SITE));
  yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
  yield* Deferred.succeed(resume, undefined);
  const exit = yield* Fiber.await(escaped);
  expect(yield* daily).toBe(2);
  if (Exit.isSuccess(exit)) expect(Number(exit.value[0]?.n)).toBe(2);
  else expect(String(exit.cause)).toContain("A Reader answers only inside");
}), 120_000);
