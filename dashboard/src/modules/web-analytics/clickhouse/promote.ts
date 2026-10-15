import { sql } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { Database } from "#/server/database.server";
import { dayStart, localDay, retentionFloor } from "../local-time";
import { LONGEST_RETENTION_MONTHS } from "../model";
import * as postgres from "../postgres/store";
import * as clickhouse from "./store";

export const promoteReads = Effect.fn("WebAnalytics.promoteReads")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.execute<{ timezone: string; rolled_up_to: string | null; dirty: string[]; reads: boolean }>(sql`
    select s.timezone, c.rolled_up_to::text as rolled_up_to, c.dirty::text[] as dirty, c.reads
    from analytics.clickhouse_site c join analytics.site s using (environment_id)
    where c.environment_id = ${environmentId}::text`, "objects");
  if (site === undefined || site.reads) return false;
  const now = yield* Clock.currentTimeMillis;
  const today = localDay(site.timezone, now);
  const yesterday = localDay(site.timezone, dayStart(site.timezone, today, -1));
  if (site.rolled_up_to === null || site.rolled_up_to < yesterday || site.dirty.length > 0) return false;
  const query: postgres.StoreQuery = {
    environmentId,
    timezone: site.timezone,
    range: { start: retentionFloor(site.timezone, now, LONGEST_RETENTION_MONTHS), end: dayStart(site.timezone, today) },
    filters: [],
  };
  const [expected, actual] = yield* Effect.all([postgres.dailyTotals(query), clickhouse.dailyTotals(query)]);
  if (JSON.stringify(expected) !== JSON.stringify(actual)) return false;
  const flipped = yield* drizzle.execute(sql`
    update analytics.clickhouse_site set reads = true, updated_at = now()
    where environment_id = ${environmentId}::text and rolled_up_to = ${site.rolled_up_to}::date and dirty = '{}'
    returning environment_id`, "objects");
  return flipped.length > 0;
});
