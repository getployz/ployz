import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, PENDING_CAP, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse, ensureBackfilled } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const SITE = "00000000-0000-4000-8000-00000000e901";
const OTHER = "00000000-0000-4000-8000-00000000e902";
const DAY = 86_400_000;
const BACKLOG = 301;

it.live("a site that lost every slot twice over recovers once ClickHouse is healthy, and its backlog never blocks another site", () => Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const day = Math.floor(Date.now() / DAY) * DAY - 3 * DAY;
  const row = (visitorId: string, on = day): PageviewRecord => ({
    ts: new Date(on + 36_000_000).toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const enqueue = (environmentId: string, id: string, on = day) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(environmentId, [row(id, on)])).returning());
  }));
  const queued = (environmentId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [queue] = yield* drizzle.execute<{ n: number }>(sql`select count(*)::int as n from analytics.outbox where environment_id = ${environmentId}::text`, "objects");
    return queue?.n ?? 0;
  }));
  const pending = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [site] = yield* drizzle.execute<{ pending: number }>(sql`
      select cardinality(pending) as pending from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
    return site?.pending ?? 0;
  }));
  const totals = (environmentId: string, on = day) => provide(Effect.map(
    store.dailyTotals({ environmentId, timezone: "UTC", range: { start: new Date(on), end: new Date(on + DAY) }, filters: [] }),
    (rows) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0),
  ));
  let lost = 0;
  const outage: ClickHouseService = { ...clickhouse, unchecked: { ...clickhouse.unchecked,
    insert: (table, rows) => clickhouse.unchecked.insert(table, rows).pipe(Effect.andThen(Effect.suspend(() => {
      lost++;
      return Effect.fail(new ClickHouseFailure({ message: "acknowledgement lost", cause: null }));
    }))),
  } };
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, [row("1"), row("2")]));
  yield* provide(backfillSite(clickhouse, SITE));
  yield* enqueue(SITE, "3");
  for (let i = 0; i < PENDING_CAP; i++) yield* provide(Effect.flip(drainOutbox(outage)), outage);
  expect(yield* pending).toBe(PENDING_CAP);
  while (lost < 2 * PENDING_CAP) yield* provide(Effect.flip(ensureBackfilled(outage, SITE)), outage);
  expect(lost, "every slot the cap holds was lost twice over").toBeGreaterThanOrEqual(2 * PENDING_CAP);

  for (let i = 0; i < BACKLOG; i++) yield* enqueue(SITE, String(4 + i), day - DAY);
  yield* provide(seedSite({ environmentId: OTHER, timezone: "UTC" }, [row("1"), row("2")]));
  yield* provide(backfillSite(clickhouse, OTHER));
  yield* enqueue(OTHER, "3");
  yield* provide(drainOutbox(clickhouse));
  expect({ other: yield* queued(OTHER), site: yield* queued(SITE) }, "the healthy site drains past the parked backlog").toEqual({ other: 0, site: BACKLOG + 1 });
  expect(yield* totals(OTHER)).toBe(3);

  const recoveries: Array<number | null> = [];
  for (let i = 0; i < 3; i++) {
    yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
    recoveries.push(yield* provide(ensureBackfilled(clickhouse, SITE)));
    yield* provide(drainOutbox(clickhouse));
  }
  expect(recoveries.some((copied) => copied !== null), "a healthy backfill publishes after the outage").toBe(true);
  expect({ pending: yield* pending, queued: yield* queued(SITE), day: yield* totals(SITE), backlog: yield* totals(SITE, day - DAY) })
    .toEqual({ pending: 0, queued: 0, day: 3, backlog: BACKLOG });
}), 240_000);
