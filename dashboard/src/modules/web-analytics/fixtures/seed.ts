import { readFileSync } from "node:fs";
import { Effect } from "effect";
import { analyticsGoal, analyticsPageview, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { ensurePartitions, monthOf } from "../postgres/partitions";
import type { Fixture, FixtureSite } from "./fixture";
import type { GoalRecord, PageviewRecord } from "./reference";

/** `analytics.site` has no foreign keys, so a fixture site needs no Organization behind it. */
export const FIXTURE_ORGANIZATION = "00000000-0000-4000-8000-00000000a000";

export const loadFixture = (name: "utc" | "sydney"): Fixture =>
  // SAFETY: the file is written by generate.ts from the Fixture type and read back untouched.
  JSON.parse(readFileSync(new URL(`./${name}.json`, import.meta.url), "utf8")) as Fixture;

export const pageviewValues = (environmentId: string, rows: ReadonlyArray<PageviewRecord>) =>
  rows.map((row) => ({ ...row, environmentId, ts: new Date(row.ts), visitorId: BigInt(row.visitorId) }));

/** Puts a site, its goals and its raw pageviews in place the way ingest would have left them, without the rollup. */
export const seedSite = Effect.fn("WebAnalytics.seedSite")(function* (
  site: FixtureSite,
  rows: ReadonlyArray<PageviewRecord>,
  goals: ReadonlyArray<GoalRecord> = [],
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(analyticsSite)
    .values({ organizationId: FIXTURE_ORGANIZATION, environmentId: site.environmentId, enabled: true, timezone: site.timezone });
  if (goals.length > 0) yield* drizzle.insert(analyticsGoal).values(goals.map((goal) => ({ ...goal, environmentId: site.environmentId })));
  yield* ensurePartitions(rows.map((row) => monthOf(new Date(row.ts))));
  if (rows.length > 0) yield* drizzle.insert(analyticsPageview).values(pageviewValues(site.environmentId, rows));
});
