import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Fiber, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as clickHouseStore from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import * as postgresStore from "#/modules/web-analytics/postgres/store";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const HOUR = 3_600_000;
const SEEDS = 12;
const STEPS = 30;

const mulberry32 = (seed: number) => () => {
  seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};

type Weighted<T> = readonly [readonly [T, number], ...ReadonlyArray<readonly [T, number]>];

const pick = <T extends string>(rng: () => number, weights: Weighted<T>): T => {
  let roll = rng() * weights.reduce((sum, [, weight]) => sum + weight, 0);
  for (const [key, weight] of weights) {
    roll -= weight;
    if (roll < 0) return key;
  }
  return weights[0][0];
};

/** What happens to one ClickHouse write: acknowledged, acknowledged but the ack lost, landing at a later step, or cut off mid-flight. */
const FATES = [["ok", 0.55], ["lost", 0.15], ["late", 0.15], ["cut", 0.15]] as const;
const STEPS_WEIGHTED = [
  ["add", 0.3], ["drain", 0.1], ["close", 0.15], ["rebuild", 0.12], ["backfill", 0.05], ["pin", 0.08], ["collect", 0.12], ["late", 0.08],
] as const;

const faults = (real: ClickHouseService, rng: () => number, trace: string[]) => {
  const late: Array<Effect.Effect<void, ClickHouseFailure>> = [];
  const daemons: Array<Fiber.Fiber<void, ClickHouseFailure>> = [];
  const landLate = Effect.suspend(() => {
    const write = late.shift();
    if (write === undefined) return Effect.succeed(false);
    trace.push("lands");
    return write.pipe(Effect.ignore, Effect.as(true));
  });
  const doomed = (what: string, write: Effect.Effect<void, ClickHouseFailure>) => Effect.suspend(() => {
    const fate = pick(rng, FATES);
    trace.push(`${what}:${fate}`);
    switch (fate) {
      case "ok": return write;
      case "lost": return write.pipe(Effect.andThen(Effect.fail(new ClickHouseFailure({ message: "acknowledgement lost", cause: null }))));
      case "late": late.push(write); return Effect.fail(new ClickHouseFailure({ message: "timed out", cause: null }));
      case "cut": return Effect.forkDetach(write).pipe(Effect.andThen((fiber) => { daemons.push(fiber); return Effect.interrupt; }));
    }
  });
  const client: ClickHouseService = {
    ...real,
    unchecked: {
      ...real.unchecked,
      insert: (table, rows): Effect.Effect<void, ClickHouseFailure> =>
        rows.length === 0 ? real.unchecked.insert(table, rows) : doomed(`insert-${table}`, real.unchecked.insert(table, rows)),
      command: (query, params): Effect.Effect<void, ClickHouseFailure> => {
        const head = query.trim().split(/\s+/, 3).join(" ");
        if (!head.startsWith("INSERT INTO")) return real.unchecked.command(query, params);
        const interleaved = head === "INSERT INTO daily" && late.length > 0 && rng() < 0.5 ? landLate : Effect.void;
        return interleaved.pipe(Effect.andThen(doomed(head.slice("INSERT INTO ".length).toLowerCase(), real.unchecked.command(query, params))));
      },
    },
  };
  const quiesce = Effect.gen(function* () {
    while (yield* landLate) {}
    yield* Effect.forEach(daemons, (fiber) => Fiber.await(fiber), { discard: true });
  });
  return { client, landLate, quiesce };
};

const total = (rows: ReadonlyArray<{ pageviews: number }>) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0);
const dayOf = (bucket: Date) => Math.floor(bucket.getTime() / DAY);

const run = (seed: number) => Effect.gen(function* () {
  forgetPartitions();
  const rng = mulberry32(seed);
  const trace: string[] = [];
  const { clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const faulty = faults(clickhouse, rng, trace);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const now = new Date(today + 12 * HOUR);
  const site = `00000000-0000-4000-8000-0000000000${(0xd0 + seed).toString(16)}`;
  const query = { environmentId: site, timezone: "UTC", range: { start: new Date(today - 4 * DAY), end: new Date(today) }, filters: [] };
  let rows = 0;
  const row = (): PageviewRecord => ({
    ts: new Date(today - (1 + Math.floor(rng() * 3)) * DAY + (1 + Math.floor(rng() * 22)) * HOUR + rows++ * 1000).toISOString(),
    visitorId: String(1 + Math.floor(rng() * 4)), host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const reads = (store: typeof clickHouseStore | typeof postgresStore) =>
    provide(Effect.all([store.dailyTotals(query), store.hourlyTotals(query), store.breakdown(query, "page")]));
  const dirtyDays = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [marker] = yield* drizzle.execute<{ dirty: string[] }>(sql`
      select dirty::text[] as dirty from analytics.clickhouse_site where environment_id = ${site}::text`, "objects");
    return new Set((marker?.dirty ?? []).map((day) => dayOf(new Date(`${day}T00:00:00Z`))));
  }));
  const enqueue = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(site, [row()])).returning());
  }));
  const step = (name: string, op: Effect.Effect<unknown>) => Effect.gen(function* () {
    trace.push(name);
    yield* Fiber.await(yield* Effect.forkChild(op));
  });
  yield* provide(seedSite({ environmentId: site, timezone: "UTC" }, [row(), row()]));
  yield* provide(backfillSite(clickhouse, site));
  let fence: number | undefined;
  let previous = yield* reads(clickHouseStore);
  for (let n = 0; n < STEPS; n++) {
    const kind = pick(rng, STEPS_WEIGHTED);
    switch (kind) {
      case "add": yield* enqueue; yield* step(kind, provide(drainOutbox(faulty.client), faulty.client)); break;
      case "drain": yield* step(kind, provide(drainOutbox(faulty.client), faulty.client)); break;
      case "close": yield* step(kind, provide(closeDays(site, now), faulty.client)); break;
      case "rebuild": yield* step(kind, provide(rebuildDirtyDays(site), faulty.client)); break;
      case "backfill": yield* step(kind, provide(backfillSite(faulty.client, site), faulty.client)); break;
      case "pin": trace.push(kind); fence = yield* provide(pin()); break;
      case "collect": yield* step(kind, provide(collectGarbage(faulty.client, fence ?? (yield* provide(pin()))), faulty.client)); break;
      case "late": trace.push(kind); yield* faulty.landLate; break;
    }
    const current = yield* reads(clickHouseStore);
    const label = `seed ${seed} after step ${n} (${kind}): ${trace.join(" ")}`;
    const [daily, hourly, pages] = current;
    const dirty = yield* dirtyDays;
    for (const day of daily) {
      if (dirty.has(dayOf(day.bucket))) continue;
      const hours = hourly.filter((hour) => dayOf(hour.bucket) === dayOf(day.bucket));
      expect({ pageviews: day.pageviews, visits: day.visits }, label).toEqual({ pageviews: total(hours), visits: hours.reduce((sum, { visits }) => sum + visits, 0) });
    }
    expect(total(pages), label).toBeGreaterThanOrEqual(total(previous[2]));
    if (kind !== "backfill") {
      expect(total(daily), label).toBeGreaterThanOrEqual(total(previous[0]));
      expect(total(hourly), label).toBeGreaterThanOrEqual(total(previous[1]));
    }
    if (kind === "collect") expect(current, label).toEqual(previous);
    previous = current;
  }
  yield* faulty.quiesce;
  for (let n = 0; n < 10 && (yield* provide(drainOutbox(clickhouse))) > 0; n++) {}
  yield* provide(rebuildDirtyDays(site));
  yield* provide(closeDays(site, now));
  yield* provide(rebuildDirtyDays(site));
  expect(yield* reads(clickHouseStore), `seed ${seed} settled: ${trace.join(" ")}`).toEqual(yield* reads(postgresStore));
});

for (let seed = 1; seed <= SEEDS; seed++) {
  it.live(`seed ${seed}: every read stays consistent while ClickHouse writes are lost, late, or cut off`, () => run(seed), 300_000);
}
