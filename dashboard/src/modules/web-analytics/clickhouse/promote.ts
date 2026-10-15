import { sql } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { Database } from "#/server/database.server";
import { dayStart, localDay, retentionFloor } from "../local-time";
import { FILTER_KEYS, LONGEST_RETENTION_MONTHS, SESSION_DIMS } from "../model";
import * as postgres from "../postgres/store";
import * as clickhouse from "./store";

const PARITY_READS = [
  [postgres.dailyTotals, clickhouse.dailyTotals],
  ...FILTER_KEYS.map((dim) => [(query: postgres.StoreQuery) => postgres.breakdown(query, dim), (query: postgres.StoreQuery) => clickhouse.breakdown(query, dim)] as const),
  ...SESSION_DIMS.map((dim) => [(query: postgres.StoreQuery) => postgres.sessionsBy(query, dim), (query: postgres.StoreQuery) => clickhouse.sessionsBy(query, dim)] as const),
  [postgres.entryPages, clickhouse.entryPages],
] as const;

const canonical = (rows: ReadonlyArray<object>) =>
  rows.map((row) => JSON.stringify(Object.fromEntries(Object.entries(row).sort(([a], [b]) => a.localeCompare(b))))).sort().join("\n");

export const sameRows = (expected: ReadonlyArray<object>, actual: ReadonlyArray<object>) => canonical(expected) === canonical(actual);

export const promoteReads = Effect.fn("WebAnalytics.promoteReads")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.execute<{ timezone: string; rolled_up_to: string | null; dirty: string[]; reads: boolean; backfilled: boolean }>(sql`
    select s.timezone, c.rolled_up_to::text as rolled_up_to, c.dirty::text[] as dirty, c.reads, c.backfilled_at is not null as backfilled
    from analytics.clickhouse_site c join analytics.site s using (environment_id)
    where c.environment_id = ${environmentId}::text`, "objects");
  if (site === undefined || site.reads || !site.backfilled) return false;
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
  for (const [expected, actual] of PARITY_READS) {
    const [left, right] = yield* Effect.all([expected(query), actual(query)]);
    if (!sameRows(left, right)) return false;
  }
  const flipped = yield* drizzle.execute(sql`
    update analytics.clickhouse_site set reads = true, updated_at = now()
    where environment_id = ${environmentId}::text and rolled_up_to = ${site.rolled_up_to}::date and dirty = '{}' and backfilled_at is not null
    returning environment_id`, "objects");
  return flipped.length > 0;
});
