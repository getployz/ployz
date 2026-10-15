import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";

export const monthOf = (date: Date) => date.toISOString().slice(0, 7);

export const nextMonth = (month: string) => {
  return monthOf(new Date(Date.UTC(Number(month.slice(0, 4)), Number(month.slice(5, 7)), 1)));
};

export const partitionName = (month: string) => `pageview_${month.replace("-", "")}`;

const ensured = new Set<string>();

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

export const forgetPartitions = () => ensured.clear();
