import { it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer } from "effect";
import { expect } from "vitest";
import * as adapter from "#/modules/web-analytics/adapter";
import { collectGarbage, PENDING_CAP, pin, read, type Reader } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService, type Params } from "#/modules/web-analytics/clickhouse/client";
import { promoteReads } from "#/modules/web-analytics/clickhouse/promote";
import { closeDays } from "#/modules/web-analytics/clickhouse/rollup";
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

const setup = Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse, url } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true", CLICKHOUSE_URL: url.href, ANALYTICS_STORE: "clickhouse" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const row = (visitorId: string): PageviewRecord => ({
    ts: new Date(today - 3 * DAY + 10 * HOUR).toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const query = { environmentId: SITE, timezone: "UTC", range: { start: new Date(today - 3 * DAY), end: new Date(today - 2 * DAY) }, filters: [] };
  const overview = adapter.overview({
    site: { environmentId: SITE, timezone: "UTC", retentionMonths: 13 },
    range: query.range,
    filters: [{ key: "host", op: "eq", value: "shop.example" }, { key: "page", op: "eq", value: "/" }],
    limit: 10,
  });
  const enqueue = (visitorId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(SITE, [row(visitorId)])).returning());
  }));
  const published = provide(Effect.gen(function* () {
    yield* closeDays(SITE, new Date(today + 12 * HOUR));
    expect(yield* promoteReads(SITE)).toBe(true);
    expect((yield* overview).totals.pageviews).toBe(2);
    yield* enqueue("3");
  }));
  const lossy = losesEveryAck(clickhouse);
  const loseDrains = (count: number) => Effect.forEach(Array.from({ length: count }), () => provide(Effect.ignore(drainOutbox(lossy)), lossy));
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, [row("1"), row("2")]));
  yield* provide(backfillSite(clickhouse, SITE));
  return { clickhouse, provide, query, overview, published, loseDrains };
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
    select: <A>(query: string, params?: Params) =>
      Deferred.succeed(captured, undefined).pipe(Effect.andThen(Deferred.await(resume)), Effect.andThen(real.unchecked.select<A>(query, params))),
  },
});

const defect = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.catchDefect((cause) => Effect.fail(String(cause))), Effect.flip, Effect.map(String));

const binds = (reader: Reader, query: Parameters<typeof store.binder>[0]) => {
  const b = store.binder(query, reader.snapshot);
  return reader.select<{ n: number }>(`SELECT count() AS n FROM pageview WHERE environment_id = ${b.env} AND ${b.visible}`, b.params);
};

it.live("a read in flight across a full pending cap and a garbage collection keeps every published pageview", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, overview, published, loseDrains } = yield* setup;
    yield* published;
    yield* loseDrains(PENDING_CAP);
    const captured = yield* Deferred.make<void>();
    const resume = yield* Deferred.make<void>();
    const reading = yield* Effect.forkChild(provide(overview, stallsFirstQuery(clickhouse, captured, resume)));
    yield* Deferred.await(captured);
    yield* loseDrains(1);
    yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
    yield* Deferred.succeed(resume, undefined);
    expect((yield* Fiber.join(reading)).totals.pageviews).toBeGreaterThanOrEqual(2);
    expect((yield* provide(overview)).totals.pageviews).toBeGreaterThanOrEqual(2);
  }), 300_000);

it.live("a read routed to ClickHouse before a drain hits the full pending cap keeps every published pageview without any garbage collection", () =>
  Effect.gen(function* () {
    const { provide, overview, published, loseDrains } = yield* setup;
    yield* published;
    yield* loseDrains(PENDING_CAP);
    const captured = yield* Deferred.make<void>();
    const resume = yield* Deferred.make<void>();
    const database = yield* provide(Database);
    let first = true;
    const { drizzle } = database;
    const gated: typeof drizzle = Object.assign(Object.create(drizzle) as typeof drizzle, {
      execute: (...args: Parameters<typeof drizzle.execute>) => Effect.gen(function* () {
        if (first) {
          first = false;
          yield* Deferred.succeed(captured, undefined);
          yield* Deferred.await(resume);
        }
        return yield* drizzle.execute(...args);
      }),
    });
    const reading = yield* Effect.forkChild(provide(overview.pipe(Effect.provideService(Database, { ...database, drizzle: gated }))));
    yield* Deferred.await(captured);
    yield* loseDrains(1);
    yield* Deferred.succeed(resume, undefined);
    expect((yield* Fiber.join(reading)).totals.pageviews).toBeGreaterThanOrEqual(2);
    expect((yield* provide(overview)).totals.pageviews).toBeGreaterThanOrEqual(2);
  }), 300_000);

it.live("a Reader returned out of its read callback answers nothing after a backfill and garbage collection moved the site", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, query } = yield* setup;
    const escaped = yield* provide(read(clickhouse, SITE, Effect.succeed));
    yield* provide(backfillSite(clickhouse, SITE));
    yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
    expect(yield* defect(binds(escaped, query))).toContain("only inside the read or locked callback");
  }), 300_000);

it.live("a read inside a repeatable-read transaction is refused, because that snapshot would hide the generation bump it checks for", () =>
  Effect.gen(function* () {
    const { provide, query } = yield* setup;
    const database = yield* provide(Database);
    const inside = database.transaction(store.dailyTotals(query), { isolationLevel: "repeatable read" });
    expect(yield* defect(provide(inside))).toContain("must not run inside one");
  }), 300_000);
