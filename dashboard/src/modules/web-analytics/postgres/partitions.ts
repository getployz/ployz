import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";

/** `YYYY-MM` of the UTC month holding `date`. `analytics.pageview` is partitioned by UTC month. */
export const monthOf = (date: Date) => date.toISOString().slice(0, 7);

/** The `YYYY-MM` that follows `month`. */
export const nextMonth = (month: string) => {
  return monthOf(new Date(Date.UTC(Number(month.slice(0, 4)), Number(month.slice(5, 7)), 1)));
};

/** `pageview_YYYYMM`, the child table of `analytics.pageview` holding the UTC month `month`. */
export const partitionName = (month: string) => `pageview_${month.replace("-", "")}`;

/** Months whose partition this process already created. A dropped partition is never recreated by a later insert of
 * old rows, so the cache only ever saves work on the hot path. */
const ensured = new Set<string>();

/** Creates the monthly partitions of `analytics.pageview` for `months` (`YYYY-MM`). Idempotent. */
export const ensurePartitions = Effect.fn("ensurePartitions")(function* (months: ReadonlyArray<string>) {
  const missing = [...new Set(months)].filter((month) => !ensured.has(month));
  if (missing.length === 0) return;
  const { drizzle } = yield* Database;
  for (const month of missing) {
    yield* drizzle.execute(sql.raw(
      `create table if not exists analytics.${partitionName(month)} partition of analytics.pageview `
      + `for values from ('${month}-01 00:00+00') to ('${nextMonth(month)}-01 00:00+00')`,
    ));
    ensured.add(month);
  }
});

/** Forgets which partitions exist, for tests that drop them. */
export const forgetPartitions = () => ensured.clear();
