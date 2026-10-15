import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { type Fixture, rangeOf, serialized, storeAnswer } from "#/modules/web-analytics/fixtures/fixture";
import { FIXTURE_ORGANIZATION, loadFixture, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { storeTestCloud } from "#/test/store-cloud";
import { ensurePartitions, forgetPartitions } from "./partitions";
import { rollupClosedDays } from "./rollup";
import { dailyTotals, dailyTotalsSql, readPlan, rolledUpTo, type StoreQuery } from "./store";

const setup = Effect.fn(function* () {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  return <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services), Effect.orDie);
});

const everyAnswerMatches = (fixture: Fixture, when: string) =>
  Effect.gen(function* () {
    for (const { spec, expected } of fixture.queries) {
      expect(serialized(yield* storeAnswer(spec, fixture.site)), `${spec.name} ${when}`).toEqual(expected);
    }
  });

const wholeQuery = (fixture: Fixture): StoreQuery => {
  const spec = fixture.queries.find((query) => query.spec.name === "overview whole")?.spec;
  if (spec === undefined) throw new Error("fixture has no 'overview whole' query");
  return { environmentId: fixture.site.environmentId, timezone: fixture.site.timezone, range: rangeOf(spec), filters: [] };
};

const CLOSED = {
  utc: { first: "2026-10-08", last: "2026-10-14" },
  sydney: { first: "2026-10-02", last: "2026-10-05" },
} as const;

for (const name of ["utc", "sydney"] as const) {
  const fixture = loadFixture(name);

  it.live(`${name}: every query answers like the reference, raw and again once the rollup has closed its days`, () =>
    Effect.gen(function* () {
      const provide = yield* setup();
      yield* provide(seedSite(fixture.site, fixture.rows, fixture.goals));
      yield* provide(everyAnswerMatches(fixture, "raw"));

      const closed = yield* provide(rollupClosedDays(new Date(fixture.now)));
      const closedThrough = yield* provide(rolledUpTo(fixture.site.environmentId));
      expect(closedThrough).toBe(CLOSED[name].last);
      expect(closed).toBeGreaterThan(0);
      const plan = readPlan(wholeQuery(fixture), closedThrough);
      expect(plan.days).toEqual(CLOSED[name]);
      expect(plan.edges).toHaveLength(1);
      yield* provide(everyAnswerMatches(fixture, "after rollup"));
    }), 120_000);
}

const perf = process.env["ANALYTICS_PERF"] === undefined ? it.live.skip : it.live;
const PERF_SITE = { environmentId: "00000000-0000-4000-8000-00000000a0ff", timezone: "Europe/Berlin" };
const PERF_ROWS = 1_000_000;

perf("with a million pageviews stored, the daily rollup closes a day under 30 s and the overview answers under a second", () =>
  Effect.gen(function* () {
    const provide = yield* setup();
    const now = new Date("2026-10-15T10:30:00Z");
    const start = new Date("2026-09-15T00:00:00Z");
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(analyticsSite)
        .values({ organizationId: FIXTURE_ORGANIZATION, environmentId: PERF_SITE.environmentId, enabled: true, timezone: PERF_SITE.timezone });
      yield* ensurePartitions(["2026-09", "2026-10"]);
      yield* drizzle.execute(sql`
        insert into analytics.pageview (environment_id, ts, visitor_id, host, pathname, source, utm_source, utm_medium, utm_campaign, language)
        select ${PERF_SITE.environmentId}::text, ${start.toISOString()}::timestamptz + random() * interval '30 days',
          (random() * 300000)::bigint, 'acme.example', '/p/' || (random() * 200)::int,
          (array['direct', 'google', 'twitter', 'github'])[1 + (random() * 3)::int],
          case when random() < 0.2 then 'newsletter' end, case when random() < 0.2 then 'email' end,
          case when random() < 0.1 then 'launch' end, (array['en', 'de', 'fr'])[1 + (random() * 2)::int]
        from generate_series(1, ${PERF_ROWS}::int)
        on conflict do nothing`);
      yield* drizzle.execute(sql`analyze analytics.pageview`);
    }));
    const query: StoreQuery = {
      environmentId: PERF_SITE.environmentId,
      timezone: PERF_SITE.timezone,
      range: { start, end: new Date("2026-10-15T11:00:00Z") },
      filters: [],
    };
    const timed = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
      Effect.gen(function* () {
        const started = performance.now();
        const result = yield* effect;
        return { result, ms: performance.now() - started };
      });

    const raw = yield* provide(timed(dailyTotals(query)));
    const backfill = yield* provide(timed(rollupClosedDays(new Date(now.getTime() - 86_400_000))));
    const rollup = yield* provide(timed(rollupClosedDays(now)));
    const rolled = yield* provide(timed(dailyTotals(query)));
    const plan = yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const closedThrough = yield* rolledUpTo(PERF_SITE.environmentId);
      return yield* drizzle.execute<{ "QUERY PLAN": string }>(
        sql`explain (analyze, buffers) ${dailyTotalsSql(query, readPlan(query, closedThrough))}`,
        "objects",
      );
    }));

    const pageviews = (rows: ReadonlyArray<{ pageviews: number }>) => rows.reduce((total, row) => total + row.pageviews, 0);
    console.log(
      `perf: ${pageviews(rolled.result)} pageviews; raw overview ${raw.ms.toFixed(0)} ms,`
        + ` backfill of ${backfill.result} days ${backfill.ms.toFixed(0)} ms, daily rollup ${rollup.ms.toFixed(0)} ms,`
        + ` overview after rollup ${rolled.ms.toFixed(0)} ms`,
    );
    console.log(plan.map((row) => row["QUERY PLAN"]).join("\n"));
    expect(pageviews(rolled.result)).toBe(pageviews(raw.result));
    expect(backfill.result + rollup.result).toBe(30);
    expect(rollup.result).toBe(1);
    expect(rollup.ms).toBeLessThan(30_000);
    expect(rolled.ms).toBeLessThan(1_000);
  }), 600_000);
