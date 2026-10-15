import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Deferred, Effect, Fiber, Layer } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import * as adapter from "#/modules/web-analytics/adapter";
import { ClickHouse, ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { promoteReads } from "#/modules/web-analytics/clickhouse/promote";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import { backfillSite, drainOutbox, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import { analyticsClickhouseSite, analyticsOutbox, analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const MATCHED = "00000000-0000-4000-8000-00000000c4a1";
const DRIFTED = "00000000-0000-4000-8000-00000000c4a2";
const RENAMED = "00000000-0000-4000-8000-00000000c4a3";
const FED_BY_OUTBOX = "00000000-0000-4000-8000-00000000c4a4";
const CUT_OFF = "00000000-0000-4000-8000-00000000c4a5";
const HOURLY_GONE = "00000000-0000-4000-8000-00000000c4a6";
const GOAL_DRIFT = "00000000-0000-4000-8000-00000000c4a7";
const PAUSED = "00000000-0000-4000-8000-00000000c4a8";

const row = (ts: Date, visitorId: string): PageviewRecord => ({
  ts: ts.toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null, utmSource: null,
  utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
});

const cloud = Effect.gen(function* () {
  forgetPartitions();
  const { url, clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({
    env: { ANALYTICS_ENABLED: "true", CLICKHOUSE_URL: url.href, ANALYTICS_STORE: "clickhouse" },
  }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
    effect.pipe(Effect.provideService(ClickHouse, clickhouse), Effect.provide(services), Effect.orDie);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const now = new Date(today + 12 * 3_600_000);
  yield* TestClock.setTime(now.getTime());
  const rows = [row(new Date(today - 3 * DAY + 10 * 3_600_000), "1"), row(new Date(today - 2 * DAY + 10 * 3_600_000), "2")];
  return { clickhouse, provide, today, now, rows };
});

const readsOf = (provide: Effect.Success<typeof cloud>["provide"]) =>
  provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ environmentId: analyticsClickhouseSite.environmentId, reads: analyticsClickhouseSite.reads })
      .from(analyticsClickhouseSite).orderBy(analyticsClickhouseSite.environmentId);
  }));

it.live("a site whose ClickHouse rollup matches Postgres flips to ClickHouse reads, and a site that differs in any served read stays on Postgres", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, now, rows } = yield* cloud;
    const postgresOnly = (environmentId: string) => provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const extra = row(new Date(today - 3 * DAY + 11 * 3_600_000), "3");
      yield* ensurePartitions([monthOf(new Date(extra.ts))]);
      yield* drizzle.insert(analyticsPageview).values(pageviewValues(environmentId, [extra]));
    }));

    for (const environmentId of [MATCHED, DRIFTED, RENAMED]) {
      yield* provide(seedSite({ environmentId, timezone: "UTC" }, rows));
      yield* provide(backfillSite(clickhouse, environmentId));
      yield* provide(closeDays(environmentId, now));
    }
    yield* postgresOnly(DRIFTED);
    yield* clickhouse.command(
      "INSERT INTO daily SELECT * REPLACE ('/renamed' AS value) FROM daily WHERE environment_id = {environmentId:String} AND dim = 'page'",
      { environmentId: RENAMED });
    yield* clickhouse.command("DELETE FROM daily WHERE environment_id = {environmentId:String} AND dim = 'page' AND value = '/'", { environmentId: RENAMED });

    expect(yield* provide(promoteReads(MATCHED))).toBe(true);
    expect(yield* provide(promoteReads(DRIFTED))).toBe(false);
    expect(yield* provide(promoteReads(RENAMED))).toBe(false);
    expect(yield* provide(promoteReads(MATCHED))).toBe(false);
    expect(yield* readsOf(provide)).toEqual([{ environmentId: MATCHED, reads: true }, { environmentId: DRIFTED, reads: false }, { environmentId: RENAMED, reads: false }]);

    yield* postgresOnly(MATCHED);
    const pageviews = (environmentId: string) => provide(adapter.overview({
      site: { environmentId, timezone: "UTC", retentionMonths: 13 },
      range: { start: new Date(today - 7 * DAY), end: new Date(today) },
      filters: [],
      limit: 10,
    })).pipe(Effect.map((answer) => answer.totals.pageviews));
    expect(yield* pageviews(MATCHED)).toBe(2);
    expect(yield* pageviews(DRIFTED)).toBe(3);
  }).pipe(Effect.provide(TestClock.layer())), 300_000);

it.live("a site fed only through the outbox stays on Postgres until a backfill has run, even when its rollup matches", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, now, rows } = yield* cloud;
    yield* provide(seedSite({ environmentId: FED_BY_OUTBOX, timezone: "UTC" }, rows));
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const stored = yield* drizzle.select().from(analyticsPageview).where(eq(analyticsPageview.environmentId, FED_BY_OUTBOX));
      yield* enqueueForClickHouse(stored);
      yield* drainOutbox(clickhouse);
    }));
    yield* provide(closeDays(FED_BY_OUTBOX, now));

    expect(yield* provide(promoteReads(FED_BY_OUTBOX))).toBe(false);
    expect(yield* readsOf(provide)).toEqual([{ environmentId: FED_BY_OUTBOX, reads: false }]);

    yield* provide(backfillSite(clickhouse, FED_BY_OUTBOX));
    yield* provide(closeDays(FED_BY_OUTBOX, now));

    expect(yield* provide(promoteReads(FED_BY_OUTBOX))).toBe(true);
  }).pipe(Effect.provide(TestClock.layer())), 300_000);

it.live("while ClickHouse is unreachable a promoted site's reads fail instead of answering from Postgres, ingest keeps queueing, and the queue lands once ClickHouse is back", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, now, rows } = yield* cloud;
    const unreachable = Effect.fail(new ClickHouseFailure({ message: "connect ECONNREFUSED", cause: null }));
    const down: ClickHouseService = { query: () => unreachable, command: () => unreachable, insert: () => unreachable };
    const during = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provideService(ClickHouse, down), Effect.flip);
    const overview = adapter.overview({
      site: { environmentId: CUT_OFF, timezone: "UTC", retentionMonths: 13 },
      range: { start: new Date(today - 7 * DAY), end: new Date(today) },
      filters: [],
      limit: 10,
    });
    const queued = Effect.gen(function* () {
      const { drizzle } = yield* Database;
      return (yield* drizzle.select().from(analyticsOutbox)).length;
    });
    yield* provide(seedSite({ environmentId: CUT_OFF, timezone: "UTC" }, rows));
    yield* provide(backfillSite(clickhouse, CUT_OFF));
    yield* provide(closeDays(CUT_OFF, now));
    expect(yield* provide(promoteReads(CUT_OFF))).toBe(true);
    expect((yield* provide(overview)).totals.pageviews).toBe(2);

    expect(yield* provide(during(overview))).toBeInstanceOf(ClickHouseFailure);
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const late = row(new Date(today - 3 * DAY + 11 * 3_600_000), "3");
      yield* ensurePartitions([monthOf(new Date(late.ts))]);
      yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(CUT_OFF, [late])).returning());
    }));
    expect(yield* provide(during(drainOutbox(down)))).toBeInstanceOf(ClickHouseFailure);
    expect(yield* provide(queued)).toBe(1);
    expect(yield* provide(during(overview))).toBeInstanceOf(ClickHouseFailure);

    expect(yield* provide(drainOutbox(clickhouse))).toBe(1);
    expect(yield* provide(queued)).toBe(0);
    yield* provide(rebuildDirtyDays(CUT_OFF));
    expect((yield* provide(overview)).totals.pageviews).toBe(3);
  }).pipe(Effect.provide(TestClock.layer())), 300_000);

it.live("a site whose hourly rollup or goal conversions differ from Postgres stays on Postgres", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, now, rows } = yield* cloud;
    const home = { id: "00000000-0000-4000-8000-00000000c4b1", name: "home", pathPattern: "/" };
    for (const [environmentId, goals] of [[HOURLY_GONE, []], [GOAL_DRIFT, [home]]] as const) {
      yield* provide(seedSite({ environmentId, timezone: "UTC" }, rows, goals));
      yield* provide(backfillSite(clickhouse, environmentId));
      yield* provide(closeDays(environmentId, now));
    }
    yield* clickhouse.command("DELETE FROM session_hourly WHERE environment_id = {environmentId:String}", { environmentId: HOURLY_GONE });
    yield* clickhouse.command("DELETE FROM pageview WHERE environment_id = {environmentId:String}", { environmentId: GOAL_DRIFT });

    expect(yield* provide(promoteReads(HOURLY_GONE))).toBe(false);
    expect(yield* provide(promoteReads(GOAL_DRIFT))).toBe(false);
  }).pipe(Effect.provide(TestClock.layer())), 300_000);

it.live("a read that routed to ClickHouse before a backfill of its site still answers from the copy it routed to", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, now, rows } = yield* cloud;
    yield* provide(seedSite({ environmentId: PAUSED, timezone: "UTC" }, rows));
    yield* provide(backfillSite(clickhouse, PAUSED));
    yield* provide(closeDays(PAUSED, now));
    expect(yield* provide(promoteReads(PAUSED))).toBe(true);

    const paused = yield* Deferred.make<void>();
    const release = yield* Deferred.make<void>();
    const gated: ClickHouseService = {
      ...clickhouse,
      query: <A>(query: string, params?: Parameters<ClickHouseService["query"]>[1]) => Effect.gen(function* () {
        if (yield* Deferred.succeed(paused, undefined)) yield* Deferred.await(release);
        return yield* clickhouse.query<A>(query, params);
      }),
    };
    const reader = yield* Effect.forkChild(provide(adapter.overview({
      site: { environmentId: PAUSED, timezone: "UTC", retentionMonths: 13 },
      range: { start: new Date(today - 7 * DAY), end: new Date(today) },
      filters: [],
      limit: 10,
    }).pipe(Effect.provideService(ClickHouse, gated))));
    yield* Deferred.await(paused);
    yield* provide(backfillSite(clickhouse, PAUSED));
    yield* Deferred.succeed(release, undefined);

    expect((yield* Fiber.join(reader)).totals.pageviews).toBe(2);
  }).pipe(Effect.provide(TestClock.layer())), 300_000);
