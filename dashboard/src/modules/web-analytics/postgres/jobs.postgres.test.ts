import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { ingestPageviewPush } from "#/modules/observe-ingest/ingest.server";
import type { PageviewInput } from "#/modules/observe-ingest/ingest";
import * as compute from "#/modules/web-analytics/compute";
import { expectedOf, type Fixture, type FixtureSite, type QuerySpec, type Serialized, serialized, storeAnswer } from "#/modules/web-analytics/fixtures/fixture";
import { loadFixture, pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import type { GoalRecord, PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { localDay } from "#/modules/web-analytics/local-time";
import { namespacesByEnvironment, setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { analyticsGoal, analyticsPageview, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { ensurePartitions, forgetPartitions } from "./partitions";
import { applyRetention, rebuildDirtyDays, rollupClosedDays } from "./rollup";
import { rolledUpTo } from "./store";

const setup = Effect.fn(function* () {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  return <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services), Effect.orDie);
});

const wholeOverview = (fixture: Fixture) => {
  const whole = fixture.queries.find((candidate) => candidate.spec.name === "overview whole");
  if (whole === undefined) throw new Error("fixture has no 'overview whole' query");
  return whole;
};

type Bucket = Serialized<ReturnType<typeof compute.timeseries>>[number];

const expectedWhileDirty = (spec: QuerySpec, site: FixtureSite, rows: ReadonlyArray<PageviewRecord>, late: ReadonlyArray<PageviewRecord>, goals: ReadonlyArray<GoalRecord>) => {
  const rolled = serialized(expectedOf(spec, site.timezone, rows, goals));
  if (late.length === 0) return rolled;
  const raw = serialized(expectedOf(spec, site.timezone, [...rows, ...late], goals));
  if (spec.filters.length > 0) return raw;
  if (spec.kind !== "timeseries" || spec.interval !== "hour") return rolled;
  return (rolled as Bucket[]).map((bucket, i) => {
    const seen = (raw as Bucket[])[i];
    if (seen === undefined) throw new Error(`raw hour timeseries has no bucket ${i}`);
    return { ...bucket, pageviews: seen.pageviews, visitors: seen.visitors };
  });
};

const everyAnswerMatches = (site: FixtureSite, fixture: Fixture, rows: ReadonlyArray<PageviewRecord>, goals: ReadonlyArray<GoalRecord>, when: string, late: ReadonlyArray<PageviewRecord> = []) =>
  Effect.gen(function* () {
    for (const { spec } of fixture.queries) {
      expect(serialized(yield* storeAnswer(spec, site)), `${spec.name} ${when}`).toEqual(expectedWhileDirty(spec, site, rows, late, goals));
    }
  });

const rollupState = Effect.fn(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ rolled_up_to: string | null; dirty: string[] }>(sql`
    select rolled_up_to::text as rolled_up_to, dirty::text[] as dirty from analytics.rollup_state
    where environment_id = ${environmentId}::text`, "objects");
  return rows[0];
});

const pageviewCount = Effect.fn(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ count: string }>(sql`
    select count(*)::bigint as count from analytics.pageview where environment_id = ${environmentId}::text`, "objects");
  return Number(rows[0]?.count);
});

it.live("a pageview arriving late for a closed day on a DST site rebuilds only that day and the answers agree with the reference again", () =>
  Effect.gen(function* () {
    const fixture = loadFixture("sydney");
    const { site, goals, rows } = fixture;
    const day = (row: PageviewRecord) => localDay(site.timezone, new Date(row.ts));
    const lateDay = "2026-10-04";
    const neighbours = new Set(rows.filter((row) => ["2026-10-03", "2026-10-05"].includes(day(row))).map((row) => row.visitorId));
    const late = rows.filter((row) => day(row) === lateDay && !neighbours.has(row.visitorId));
    const onTime = rows.filter((row) => !late.includes(row));
    expect(late.length).toBeGreaterThan(0);

    const provide = yield* setup();
    yield* provide(seedSite(site, onTime, goals));
    yield* provide(rollupClosedDays(new Date(fixture.now)));
    expect(yield* provide(rolledUpTo(site.environmentId))).toBe("2026-10-05");
    yield* provide(everyAnswerMatches(site, fixture, onTime, goals, "before the late rows"));
    const whole = wholeOverview(fixture);
    expect(serialized(yield* provide(storeAnswer(whole.spec, site)))).not.toEqual(whole.expected);

    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(analyticsPageview).values(pageviewValues(site.environmentId, late));
      yield* drizzle.execute(sql`
        update analytics.rollup_state set dirty = array[${lateDay}::date] where environment_id = ${site.environmentId}::text`);
    }));
    expect(yield* provide(rebuildDirtyDays())).toBe(1);
    expect(yield* provide(rollupState(site.environmentId))).toEqual({ rolled_up_to: "2026-10-05", dirty: [] });
    for (const { spec, expected } of fixture.queries) {
      expect(serialized(yield* provide(storeAnswer(spec, site))), `${spec.name} after the rebuild`).toEqual(expected);
    }
  }), 120_000);

const ORGANIZATION = "00000000-0000-4000-8000-00000000f501";
const PROJECT = "00000000-0000-4000-8000-00000000f502";
const PRODUCTION = "00000000-0000-4000-8000-00000000f503";

it.live("a batch replayed through ingest adds nothing, and late pageviews dirty the days they touch until the rebuild", () =>
  Effect.gen(function* () {
    const fixture = loadFixture("utc");
    const site: FixtureSite = { environmentId: PRODUCTION, timezone: fixture.site.timezone };
    yield* TestClock.setTime(Date.parse(fixture.now));
    const provide = yield* setup();
    yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
    yield* provide(enrollStoreServer(ORGANIZATION));
    const store = yield* provide(cloudStore);
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: PRODUCTION }));
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`
        insert into config_namespace (organization_id, namespace, environment_id) values (${ORGANIZATION}, 'shop-prod', ${PRODUCTION})`);
      yield* drizzle.insert(analyticsGoal).values(fixture.goals.map((goal) => ({ ...goal, environmentId: PRODUCTION })));
    }));
    yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: site.timezone }));
    const namespace = (yield* provide(namespacesByEnvironment(ORGANIZATION))).get(PRODUCTION);
    if (namespace === undefined) throw new Error("production has no namespace");

    const pusher = { organizationId: ORGANIZATION, machineId: machineId("0") };
    const event = (row: PageviewRecord): PageviewInput =>
      ({ ...row, kind: "pageview", namespace, ts: new Date(row.ts), visitorId: BigInt(row.visitorId), flags: 0 });
    const push = (seq: bigint, rows: ReadonlyArray<PageviewRecord>) =>
      provide(ingestPageviewPush(pusher, { seq, dropped: 0, events: rows.map(event) }));

    yield* push(1n, fixture.rows);
    yield* push(1n, fixture.rows);
    expect(yield* provide(pageviewCount(PRODUCTION))).toBe(fixture.rows.length);
    expect(yield* provide(rollupClosedDays(new Date(fixture.now)))).toBe(7);
    yield* push(1n, fixture.rows);
    expect(yield* provide(pageviewCount(PRODUCTION))).toBe(fixture.rows.length);
    expect(yield* provide(rollupState(PRODUCTION))).toEqual({ rolled_up_to: "2026-10-14", dirty: [] });

    const template = fixture.rows[0];
    if (template === undefined) throw new Error("fixture has no rows");
    const late: PageviewRecord[] = [
      { ...template, visitorId: "777", pathname: "/late", ts: "2026-10-10T12:00:00.000Z" },
      { ...template, visitorId: "777", pathname: "/late", ts: "2026-10-12T00:10:00.000Z" },
    ];
    yield* push(2n, late);
    expect(yield* provide(pageviewCount(PRODUCTION))).toBe(fixture.rows.length + late.length);
    expect(yield* provide(rollupState(PRODUCTION))).toEqual({ rolled_up_to: "2026-10-14", dirty: ["2026-10-10", "2026-10-11", "2026-10-12"] });
    yield* provide(everyAnswerMatches(site, fixture, fixture.rows, fixture.goals, "while the late days are still dirty", late));

    expect(yield* provide(rebuildDirtyDays())).toBe(3);
    expect(yield* provide(rollupState(PRODUCTION))).toEqual({ rolled_up_to: "2026-10-14", dirty: [] });
    yield* provide(everyAnswerMatches(site, fixture, [...fixture.rows, ...late], fixture.goals, "after the rebuild"));
  }).pipe(Effect.provide(TestClock.layer())), 120_000);

const partitions = Effect.fn(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ name: string }>(sql`
    select c.relname as name from pg_inherits i join pg_class c on c.oid = i.inhrelid join pg_class p on p.oid = i.inhparent
    join pg_namespace n on n.oid = p.relnamespace where n.nspname = 'analytics' and p.relname = 'pageview' order by 1`, "objects");
  return rows.map((row) => row.name);
});

it.live("retention drops only the partition whose whole month is past the longest retention and trims rollups to the floor", () =>
  Effect.gen(function* () {
    const provide = yield* setup();
    const site = { organizationId: "00000000-0000-4000-8000-00000000a000", environmentId: "00000000-0000-4000-8000-00000000a0f0", enabled: true, timezone: "UTC" };
    const row = (ts: string) => ({ ts, visitorId: "1", host: "acme.example", pathname: "/", source: "direct", referrerHost: null, utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: null });
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(analyticsSite).values(site);
      yield* ensurePartitions(["2025-08", "2025-09", "2026-10"]);
      yield* drizzle.insert(analyticsPageview)
        .values(pageviewValues(site.environmentId, [row("2025-08-20T10:00:00Z"), row("2025-09-20T10:00:00Z"), row("2026-10-01T10:00:00Z")]));
      yield* drizzle.execute(sql`
        insert into analytics.daily (environment_id, day, dim, value, pageviews, visitors, visits, bounces, total_duration)
        select ${site.environmentId}::text, d::date, '', '', 1, 1, 1, 0, 0 from unnest(array['2025-08-15', '2025-09-14', '2025-09-15', '2026-10-01']) d`);
      yield* drizzle.execute(sql`
        insert into analytics.session_hourly (environment_id, hour, visits, bounces, total_duration)
        select ${site.environmentId}::text, h::timestamptz, 1, 0, 0 from unnest(array['2025-09-14T23:00:00Z', '2025-09-15T00:00:00Z']) h`);
    }));

    expect(yield* provide(applyRetention(new Date("2026-10-15T10:30:00Z")))).toEqual(["pageview_202508"]);
    expect(yield* provide(partitions())).toEqual(["pageview_202509", "pageview_202610", "pageview_202611", "pageview_202612"]);
    expect(yield* provide(pageviewCount(site.environmentId))).toBe(2);
    const kept = yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const days = yield* drizzle.execute<{ day: string }>(sql`
        select day::text as day from analytics.daily where environment_id = ${site.environmentId}::text order by 1`, "objects");
      const hours = yield* drizzle.execute<{ hour: string }>(sql`
        select to_char(hour at time zone 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as hour from analytics.session_hourly
        where environment_id = ${site.environmentId}::text order by 1`, "objects");
      return { days: days.map((row) => row.day), hours: hours.map((row) => row.hour) };
    }));
    expect(kept).toEqual({ days: ["2025-09-15", "2026-10-01"], hours: ["2025-09-15T00:00:00Z"] });
  }), 60_000);
