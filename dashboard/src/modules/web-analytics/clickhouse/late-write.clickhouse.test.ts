import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Deferred, Effect, Exit, Fiber, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, mint, pin, read } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, clickHouseClient, makeClickHouse, type ClickHouseService, type Params } from "#/modules/web-analytics/clickhouse/client";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, clickHouseRow, drainOutbox, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database, type DatabaseService } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const HOUR = 3_600_000;
const SITE = "00000000-0000-4000-8000-00000000c6a4";

const setup = Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse, url } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
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
  const attempts = Effect.map(
    clickhouse.unchecked.select<{ attempt: string }>("SELECT DISTINCT attempt FROM pageview WHERE environment_id = {env:String} ORDER BY attempt", { env: SITE }),
    (rows) => rows.map(({ attempt }) => Number(attempt)));
  return { clickhouse, provide, daily, enqueue, pendingCount, query, url, attempts };
});

const failingSelect = (clickhouse: ClickHouseService): ClickHouseService => ({
  ...clickhouse,
  unchecked: { ...clickhouse.unchecked, select: () => Effect.fail(new ClickHouseFailure({ message: "GC failed after fence committed", cause: null })) },
});

const pausedSecondExecute = (drizzle: DatabaseService["drizzle"], before: Deferred.Deferred<void>, resume: Deferred.Deferred<void>): DatabaseService["drizzle"] => {
  let statements = 0;
  return Object.assign(Object.create(drizzle) as typeof drizzle, {
    execute: (...args: Parameters<typeof drizzle.execute>) => Effect.gen(function* () {
      if (++statements === 2) {
        yield* Deferred.succeed(before, undefined);
        yield* Deferred.await(resume);
      }
      return yield* drizzle.execute(...args);
    }),
  });
};

/** Pauses the second statement of every transaction, which for a fence transaction is the UPDATE after the row lock. */
const pausedSecondStatement = (database: DatabaseService, beforeUpdate: Deferred.Deferred<void>, resume: Deferred.Deferred<void>): DatabaseService => ({
  ...database,
  transaction: (program, config) => database.transaction(Effect.gen(function* () {
    const transaction = yield* Database;
    return yield* program.pipe(Effect.provideService(Database, { ...transaction, drizzle: pausedSecondExecute(transaction.drizzle, beforeUpdate, resume) }));
  }), config),
});

/** Pauses the second pool statement, which for a drain is the writer's fence check, so the mint has committed and the INSERT has not started. */
const pausedSecondPoolStatement = (database: DatabaseService, beforeInsert: Deferred.Deferred<void>, resume: Deferred.Deferred<void>): DatabaseService => ({
  ...database,
  drizzle: pausedSecondExecute(database.drizzle, beforeInsert, resume),
});

const insertRunning = (clickhouse: ClickHouseService) => Effect.gen(function* () {
  let running = false;
  for (let i = 0; i < 200 && !running; i++) {
    const [row] = yield* clickhouse.unchecked.select<{ n: number }>("SELECT count() AS n FROM system.processes WHERE query LIKE 'INSERT INTO%pageview%'");
    running = Number(row?.n ?? 0) > 0;
    if (!running) yield* Effect.sleep("5 millis");
  }
  return running;
});

it.live("an interrupted INSERT stays excluded through any number of collections until a publication retires it", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, enqueue, pendingCount } = yield* setup;
  yield* enqueue("3");
  const database = yield* provide(Database);
  const minted = yield* Deferred.make<void>();
  const resume = yield* Deferred.make<void>();
  yield* clickhouse.unchecked.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");
  const writing = yield* Effect.forkChild(provide(drainOutbox(clickhouse).pipe(
    Effect.provideService(Database, pausedSecondPoolStatement(database, minted, resume)))));
  yield* Deferred.await(minted);
  const fence = yield* provide(pin());
  yield* Deferred.succeed(resume, undefined);
  expect(yield* insertRunning(clickhouse)).toBe(true);
  yield* Fiber.interrupt(writing);
  yield* provide(Effect.flip(collectGarbage(failingSelect(clickhouse), fence)));
  expect(yield* pendingCount).toBe(1);
  for (let i = 0; i < 3; i++) {
    yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
    expect(yield* pendingCount).toBe(1);
    expect(yield* daily).toBe(2);
  }
  yield* provide(drainOutbox(clickhouse));
  expect(yield* daily).toBe(3);
  expect(yield* pendingCount).toBe(1);
  yield* provide(backfillSite(clickhouse, SITE));
  expect(yield* pendingCount).toBe(0);
  expect(yield* daily).toBe(3);
  yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
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

it.live("a detached Reader query cannot leak deleted rows while its callback is still open", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, query } = yield* setup;
  const started = yield* Deferred.make<void>();
  const resumeQuery = yield* Deferred.make<void>();
  const finishCallback = yield* Deferred.make<void>();
  const result = yield* Deferred.make<Exit.Exit<ReadonlyArray<{ n: number }>, ClickHouseFailure>>();
  const stalled: ClickHouseService = { ...clickhouse, unchecked: { ...clickhouse.unchecked,
    select: <A>(text: string, params?: Params) => Deferred.succeed(started, undefined).pipe(
      Effect.andThen(Deferred.await(resumeQuery)), Effect.andThen(clickhouse.unchecked.select<A>(text, params))),
  } };
  const reading = yield* Effect.forkChild(provide(read(stalled, SITE, (reader) => Effect.gen(function* () {
    const b = store.binder(query, reader.snapshot);
    yield* Effect.forkDetach(reader.select<{ n: number }>(`SELECT count() AS n FROM pageview WHERE environment_id = ${b.env} AND ${b.visible}`, b.params).pipe(
      Effect.exit, Effect.flatMap((exit) => Deferred.succeed(result, exit))));
    yield* Deferred.await(finishCallback);
  }))));
  yield* Deferred.await(started);
  yield* provide(backfillSite(clickhouse, SITE));
  yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
  yield* Deferred.succeed(resumeQuery, undefined);
  const observed = yield* Deferred.await(result);
  const actual = Exit.isSuccess(observed) ? Number(observed.value[0]?.n) : "rejected";
  yield* Fiber.interrupt(reading);
  expect(yield* daily).toBe(2);
  expect(actual === "rejected" || actual === 2).toBe(true);
}), 120_000);

it.live("a production client INSERT queued behind busy sockets lands excluded however long it waited", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, enqueue, pendingCount, url } = yield* setup;
  const queued = makeClickHouse(yield* clickHouseClient(url.href));
  yield* enqueue("3");
  const database = yield* provide(Database);
  const minted = yield* Deferred.make<void>();
  const resumeWriter = yield* Deferred.make<void>();
  const started = yield* Deferred.make<void>();
  const observed: ClickHouseService = { ...queued, unchecked: { ...queued.unchecked,
    insert: (table, rows) => Deferred.succeed(started, undefined).pipe(Effect.andThen(queued.unchecked.insert(table, rows))),
  } };
  const writing = yield* Effect.forkChild(provide(drainOutbox(observed).pipe(
    Effect.provideService(Database, pausedSecondPoolStatement(database, minted, resumeWriter)))));
  yield* Deferred.await(minted);
  const fence = yield* provide(pin());
  const blockers = yield* Effect.forEach(Array.from({ length: 30 }), (_, index) => Effect.forkChild(
    queued.unchecked.select(`SELECT sleepEachRow(3) AS n FROM numbers(8) SETTINGS max_block_size = 1, max_threads = 1 /* round8-blocker-${index} */`).pipe(Effect.exit)));
  let active = 0;
  for (let i = 0; i < 200 && active < 10; i++) {
    const [row] = yield* clickhouse.unchecked.select<{ n: number }>("SELECT count() AS n FROM system.processes WHERE query LIKE '%round8-blocker-%' AND query NOT LIKE '%system.processes%'");
    active = Number(row?.n ?? 0);
    if (active < 10) yield* Effect.sleep("10 millis");
  }
  expect(active).toBe(10);
  yield* Deferred.succeed(resumeWriter, undefined);
  yield* Deferred.await(started);
  yield* Effect.sleep("100 millis");
  yield* Fiber.interrupt(writing);
  yield* provide(Effect.flip(collectGarbage(failingSelect(clickhouse), fence)));
  expect(yield* pendingCount).toBe(1);
  yield* Effect.sleep("51 seconds");
  yield* provide(collectGarbage(clickhouse, fence));
  expect(yield* pendingCount).toBe(1);
  yield* provide(drainOutbox(clickhouse));
  const before = yield* daily;
  yield* Effect.forEach(blockers, Fiber.join, { discard: true });
  let after = before;
  for (let i = 0; i < 40 && after === before; i++) {
    yield* Effect.sleep("250 millis");
    after = yield* daily;
  }
  yield* provide(backfillSite(clickhouse, SITE));
  const replaced = yield* daily;
  expect({ before, after, replaced }).toEqual({ before: 3, after: 3, replaced: 3 });
}), 150_000);

it.live("a fence that commits long after its transaction began excludes an INSERT still running at commit", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, enqueue, pendingCount } = yield* setup;
  yield* enqueue("3");
  const database = yield* provide(Database);
  const writer = (yield* provide(mint(clickhouse, [SITE]))).get(SITE);
  if (writer === undefined) return yield* Effect.die("missing minted writer");
  const fence = yield* provide(pin());
  const beforeUpdate = yield* Deferred.make<void>();
  const resume = yield* Deferred.make<void>();
  const collecting = yield* Effect.forkChild(provide(collectGarbage(failingSelect(clickhouse), fence).pipe(
    Effect.flip, Effect.provideService(Database, pausedSecondStatement(database, beforeUpdate, resume)))));
  yield* Deferred.await(beforeUpdate);
  yield* Effect.sleep("51 seconds");
  yield* clickhouse.unchecked.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");
  const [row] = yield* provide(database.drizzle.select().from(analyticsPageview).where(sql`${analyticsPageview.environmentId} = ${SITE} and ${analyticsPageview.visitorId} = 3`));
  if (row === undefined) return yield* Effect.die("missing queued pageview");
  const writing = yield* Effect.forkChild(provide(writer.rows("pageview", [clickHouseRow(row)])));
  expect(yield* insertRunning(clickhouse)).toBe(true);
  yield* Fiber.interrupt(writing);
  yield* Deferred.succeed(resume, undefined);
  yield* Fiber.join(collecting);
  expect(yield* pendingCount).toBe(1);
  yield* provide(collectGarbage(clickhouse, fence));
  const pendingAfterRetry = yield* pendingCount;
  yield* provide(drainOutbox(clickhouse));
  const after = yield* daily;
  yield* provide(backfillSite(clickhouse, SITE));
  const replaced = yield* daily;
  expect({ pendingAfterRetry, after, replaced }).toEqual({ pendingAfterRetry: 1, after: 3, replaced: 3 });
}), 120_000);

it.live("garbage collection that fails after fencing or after deleting reaches the state of one clean run however often it is retried", () => Effect.gen(function* () {
  const { clickhouse, provide, daily, enqueue, pendingCount, attempts } = yield* setup;
  yield* enqueue("3");
  const lossy: ClickHouseService = { ...clickhouse, unchecked: { ...clickhouse.unchecked,
    insert: (table, rows) => clickhouse.unchecked.insert(table, rows).pipe(
      Effect.andThen(Effect.fail(new ClickHouseFailure({ message: "ack lost", cause: null })))),
  } };
  yield* provide(Effect.ignore(drainOutbox(lossy)), lossy);
  expect(yield* pendingCount).toBe(1);
  expect(yield* daily).toBe(2);
  yield* provide(backfillSite(clickhouse, SITE));
  expect(yield* pendingCount).toBe(0);
  expect(yield* daily).toBe(3);
  const [floor] = yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.execute<{ floor: string }>(sql`select floor::text from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
  }));
  const published = Number(floor?.floor);
  expect((yield* attempts).length).toBeGreaterThan(1);
  const deleteThenFail: ClickHouseService = { ...clickhouse, unchecked: { ...clickhouse.unchecked,
    command: (text, params) => clickhouse.unchecked.command(text, params).pipe(
      Effect.andThen(Effect.fail(new ClickHouseFailure({ message: "GC failed after deleting", cause: null })))),
  } };
  for (let round = 0; round < 3; round++) {
    const fence = yield* provide(pin());
    yield* provide(Effect.flip(collectGarbage(failingSelect(clickhouse), fence)));
    yield* provide(Effect.flip(collectGarbage(failingSelect(clickhouse), fence)));
    yield* provide(Effect.ignore(collectGarbage(deleteThenFail, fence)));
    expect(yield* daily).toBe(3);
  }
  yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
  expect(yield* attempts).toEqual([published]);
  expect(yield* daily).toBe(3);
  expect(yield* pendingCount).toBe(0);
  expect(yield* provide(collectGarbage(clickhouse, yield* provide(pin())))).toBe(0);
  expect(yield* attempts).toEqual([published]);
}), 120_000);
