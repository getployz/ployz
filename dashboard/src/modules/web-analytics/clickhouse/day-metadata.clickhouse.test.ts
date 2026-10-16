import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { ClickHouse } from "#/modules/web-analytics/clickhouse/client";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;

it.live("a current-range read stays under the ClickHouse field limit however many days a site has published", () => Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provideService(ClickHouse, clickhouse), Effect.provide(services), Effect.orDie);
  const site = "00000000-0000-4000-8000-00000000e903";
  const day = Math.floor(Date.now() / DAY) * DAY - 3 * DAY;
  const row: PageviewRecord = {
    ts: new Date(day + 36_000_000).toISOString(), visitorId: "1", host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  };
  yield* provide(seedSite({ environmentId: site, timezone: "UTC" }, [row]));
  yield* provide(backfillSite(clickhouse, site));
  for (const count of [400, 1_000, 4_000, 16_000]) {
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`
        update analytics.clickhouse_site set days = (
          select jsonb_object_agg((current_date - i)::text, i + 100)
          from generate_series(1, ${count}::int) as i)
        where environment_id = ${site}`);
    }));
    const totals = yield* provide(Effect.exit(store.dailyTotals({ environmentId: site, timezone: "UTC", range: { start: new Date(day), end: new Date(day + DAY) }, filters: [] })));
    expect(totals._tag, `a current-range read with ${count} published day markers`).toBe("Success");
  }
}), 60_000);
