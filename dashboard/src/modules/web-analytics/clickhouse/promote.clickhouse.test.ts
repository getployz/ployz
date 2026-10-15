import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import * as adapter from "#/modules/web-analytics/adapter";
import { ClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { promoteReads } from "#/modules/web-analytics/clickhouse/promote";
import { closeDays } from "#/modules/web-analytics/clickhouse/rollup";
import { backfillSite } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import { analyticsClickhouseSite, analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const MATCHED = "00000000-0000-4000-8000-00000000c4a1";
const DRIFTED = "00000000-0000-4000-8000-00000000c4a2";

const row = (ts: Date, visitorId: string): PageviewRecord => ({
  ts: ts.toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null, utmSource: null,
  utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
});

it.live("a site whose ClickHouse rollup matches Postgres flips to ClickHouse reads, and a site that differs stays on Postgres", () =>
  Effect.gen(function* () {
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
    const postgresOnly = (environmentId: string) => provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const extra = row(new Date(today - 3 * DAY + 11 * 3_600_000), "3");
      yield* ensurePartitions([monthOf(new Date(extra.ts))]);
      yield* drizzle.insert(analyticsPageview).values(pageviewValues(environmentId, [extra]));
    }));

    for (const environmentId of [MATCHED, DRIFTED]) {
      yield* provide(seedSite({ environmentId, timezone: "UTC" }, rows));
      yield* provide(backfillSite(clickhouse, environmentId));
      yield* provide(closeDays(environmentId, now));
    }
    yield* postgresOnly(DRIFTED);

    expect(yield* provide(promoteReads(MATCHED))).toBe(true);
    expect(yield* provide(promoteReads(DRIFTED))).toBe(false);
    expect(yield* provide(promoteReads(MATCHED))).toBe(false);
    const reads = yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      return yield* drizzle.select({ environmentId: analyticsClickhouseSite.environmentId, reads: analyticsClickhouseSite.reads })
        .from(analyticsClickhouseSite).orderBy(analyticsClickhouseSite.environmentId);
    }));
    expect(reads).toEqual([{ environmentId: MATCHED, reads: true }, { environmentId: DRIFTED, reads: false }]);

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
