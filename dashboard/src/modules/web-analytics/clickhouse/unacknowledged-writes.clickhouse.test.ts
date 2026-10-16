import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Deferred, Effect, Fiber, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, PENDING_CAP, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService, type Params } from "#/modules/web-analytics/clickhouse/client";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse, ensureBackfilled } from "#/modules/web-analytics/clickhouse/writer";
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

const setup = Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const row = (visitorId: string): PageviewRecord => ({
    ts: new Date(today - 3 * DAY + 10 * HOUR).toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const query = { environmentId: SITE, timezone: "UTC", range: { start: new Date(today - 3 * DAY), end: new Date(today - 2 * DAY) }, filters: [] };
  const total = (rows: ReadonlyArray<{ pageviews: number }>) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0);
  const daily = provide(Effect.map(store.dailyTotals(query), total));
  const hourly = provide(Effect.map(store.hourlyTotals(query), total));
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
  const recovery = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ recovery: string | null }>(sql`
      select recovery::text as recovery from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
    return row?.recovery ?? null;
  }));
  return { clickhouse, provide, today, daily, hourly, enqueue, pendingCount, recovery, query, total };
});

const losesEveryAck = (real: ClickHouseService): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    insert: (table, rows) => real.unchecked.insert(table, rows).pipe(Effect.andThen(rows.length === 0
      ? Effect.void
      : Effect.fail(new ClickHouseFailure({ message: "acknowledgement lost", cause: null })))),
  },
});

const stallsFirstQuery = (real: ClickHouseService, captured: Deferred.Deferred<void>, resume: Deferred.Deferred<void>): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    select: <A>(query: string, params?: Params) => Deferred.succeed(captured, undefined).pipe(Effect.andThen(Deferred.await(resume)), Effect.andThen(real.unchecked.select<A>(query, params))),
  },
});

const failsAfterDaily = (real: ClickHouseService): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    command: (query, params) => real.unchecked.command(query, params).pipe(Effect.andThen(query.trim().startsWith("INSERT INTO daily")
      ? Effect.fail(new ClickHouseFailure({ message: "lost rollup acknowledgement", cause: null }))
      : Effect.void)),
  },
});

const sleepsAfterHourly = (real: ClickHouseService): ClickHouseService => {
  let slept = false;
  return {
    ...real,
    unchecked: {
      ...real.unchecked,
      command: (query, params) => real.unchecked.command(query, params).pipe(Effect.andThen(Effect.suspend(() => {
        if (slept || !query.trim().startsWith("INSERT INTO session_hourly")) return Effect.void;
        slept = true;
        return Effect.sleep("4 seconds");
      }))),
    },
  };
};

it.live("a rollup whose acknowledgement was lost stays invisible after a later day publishes and GC runs with a stale pin", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, daily, enqueue } = yield* setup;
    yield* provide(closeDays(SITE, new Date(today - 2 * DAY + HOUR)));
    yield* enqueue("3");
    yield* provide(drainOutbox(clickhouse));
    yield* provide(Effect.flip(rebuildDirtyDays(SITE)), failsAfterDaily(clickhouse));
    expect(yield* daily).toBe(2);
    const fence = yield* provide(pin());
    yield* provide(closeDays(SITE, new Date(today + 12 * HOUR)));
    expect(yield* daily).toBe(2);
    yield* provide(collectGarbage(clickhouse, fence));
    expect(yield* daily).toBe(2);
    yield* provide(rebuildDirtyDays(SITE));
    expect(yield* daily).toBe(3);
  }), 300_000);

it.live("a drain interrupted mid-INSERT cannot land rows inside a rollup", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, daily, hourly, enqueue } = yield* setup;
    yield* enqueue("3");
    yield* clickhouse.unchecked.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");
    const drain = yield* Effect.forkChild(provide(drainOutbox(clickhouse)));
    for (let n = 0; n < 100; n++) {
      const [running] = yield* clickhouse.unchecked.select<{ n: number }>("SELECT count() AS n FROM system.processes WHERE query LIKE 'INSERT INTO%pageview%'");
      if (running?.n) break;
      yield* Effect.sleep("10 millis");
    }
    yield* Fiber.interrupt(drain);
    yield* provide(closeDays(SITE, new Date(today - 2 * DAY + HOUR)), sleepsAfterHourly(clickhouse));
    expect(yield* daily).toBe(yield* hourly);
    expect(yield* daily).toBe(2);
  }), 300_000);

for (const supersedes of ["a rebuild", "a backfill"] as const) {
  it.live(`a read that paused before its ClickHouse query survives ${supersedes} and a garbage collection`, () =>
    Effect.gen(function* () {
      const { clickhouse, provide, today, daily, enqueue, query, total } = yield* setup;
      if (supersedes === "a rebuild") yield* provide(closeDays(SITE, new Date(today - 2 * DAY + HOUR)));
      expect(yield* daily).toBe(2);
      const fence = yield* provide(pin());
      const captured = yield* Deferred.make<void>();
      const resume = yield* Deferred.make<void>();
      const reading = yield* Effect.forkChild(provide(Effect.map(store.dailyTotals(query), total), stallsFirstQuery(clickhouse, captured, resume)));
      yield* Deferred.await(captured);
      if (supersedes === "a rebuild") {
        yield* enqueue("3");
        yield* provide(drainOutbox(clickhouse));
        yield* provide(rebuildDirtyDays(SITE));
      } else {
        yield* provide(backfillSite(clickhouse, SITE));
      }
      expect(yield* provide(collectGarbage(clickhouse, fence))).toBeGreaterThan(0);
      yield* Deferred.succeed(resume, undefined);
      expect([2, yield* daily]).toContain(yield* Fiber.join(reading));
    }), 300_000);
}

it.live("losing every acknowledgement past the pending cap parks appends behind the published view until a backfill publishes the refloor that frees them", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, daily, enqueue, pendingCount, recovery } = yield* setup;
    const lossy = losesEveryAck(clickhouse);
    const lost = PENDING_CAP + 8;
    for (let n = 0; n < lost; n++) {
      yield* enqueue(String(3 + n));
      yield* provide(Effect.ignore(drainOutbox(lossy)), lossy);
      expect(yield* pendingCount).toBeLessThanOrEqual(PENDING_CAP);
      expect(yield* daily).toBe(2);
    }
    expect(yield* pendingCount).toBe(PENDING_CAP);
    expect(yield* provide(drainOutbox(clickhouse))).toBe(0);
    yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
    yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
    expect(yield* pendingCount).toBe(PENDING_CAP);
    expect((yield* provide(Effect.flip(ensureBackfilled(lossy, SITE)), lossy))._tag).toBe("ClickHouseFailure");
    expect(yield* pendingCount).toBe(PENDING_CAP);
    expect(yield* recovery).not.toBeNull();
    expect(yield* provide(drainOutbox(clickhouse))).toBe(0);
    expect(yield* daily).toBe(2);
    expect(yield* provide(ensureBackfilled(clickhouse, SITE))).toBe(2 + lost);
    expect(yield* pendingCount).toBe(0);
    expect(yield* recovery).toBeNull();
    expect(yield* daily).toBe(2 + lost);
    expect(yield* provide(ensureBackfilled(clickhouse, SITE))).toBeNull();
    yield* enqueue(String(3 + lost));
    expect(yield* provide(drainOutbox(clickhouse))).toBe(1);
    expect(yield* daily).toBe(3 + lost);
    expect(yield* pendingCount).toBe(0);
  }), 300_000);
