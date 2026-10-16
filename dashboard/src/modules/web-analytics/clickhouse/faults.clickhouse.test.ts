import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Deferred, Effect, Exit, Fiber, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, PENDING_CAP, pin, read } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService, type Params } from "#/modules/web-analytics/clickhouse/client";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as clickHouseStore from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse, ensureBackfilled } from "#/modules/web-analytics/clickhouse/writer";
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
/** Late writes stop landing half a second before the lifetime GC honours, so the harness clock and the Postgres clock cannot disagree about whether a write was still possible. */
const LIFETIME_SECONDS = 2;
const LATE_WINDOW_MS = (LIFETIME_SECONDS - 0.5) * 1000;
const SETTLE = `${LIFETIME_SECONDS * 1000 + 500} millis`;

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
type Fate = "ok" | "lost" | "late" | "cut";
type Step = "add" | "drain" | "close" | "rebuild" | "backfill" | "pin" | "collect" | "late" | "settle" | "read" | "escape" | "finish";
type Profile = { readonly fates: Weighted<Fate>; readonly steps: Weighted<Step>; readonly length: number; readonly seeds: ReadonlyArray<number>; readonly crossesCap?: true };

const MIXED_STEPS: Weighted<Step> = [
  ["add", 0.3], ["drain", 0.1], ["close", 0.15], ["rebuild", 0.12], ["backfill", 0.05], ["pin", 0.08], ["collect", 0.12], ["late", 0.08], ["settle", 0.05],
  ["read", 0.1], ["escape", 0.06], ["finish", 0.14],
];
/** Mints outrun collections and no refloor retires the dead slots, so the pending cap fills while reads are in flight. */
const CAPPING_STEPS: Weighted<Step> = [
  ["add", 0.43], ["drain", 0.15], ["close", 0.05], ["rebuild", 0.05], ["pin", 0.04], ["collect", 0.04], ["late", 0.04], ["settle", 0.04], ["read", 0.08],
  ["escape", 0.04], ["finish", 0.12],
];

const PROFILES: ReadonlyArray<Profile> = [
  { fates: [["ok", 0.55], ["lost", 0.15], ["late", 0.15], ["cut", 0.15]], steps: MIXED_STEPS, length: 30, seeds: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] },
  { fates: [["ok", 0.2], ["lost", 0.6], ["late", 0.1], ["cut", 0.1]], steps: MIXED_STEPS, length: 150, seeds: [13, 14, 15, 16] },
  { fates: [["ok", 0.1], ["lost", 0.8], ["late", 0.05], ["cut", 0.05]], steps: CAPPING_STEPS, length: 120, seeds: [17, 18, 19, 20], crossesCap: true },
];

type Stamped = { readonly attempt?: Params[string] };
const attemptOf = (stamped: Stamped | undefined) => Number(stamped?.attempt);

const faults = (real: ClickHouseService, rng: () => number, trace: string[], fates: Weighted<Fate>) => {
  type Late = { readonly attempt: number; readonly deadline: number; readonly write: Effect.Effect<void, ClickHouseFailure> };
  const late: Late[] = [];
  const daemons: Array<{ readonly attempt: number; readonly fiber: Fiber.Fiber<void, ClickHouseFailure> }> = [];
  let gcFailures = 0;
  const land = ({ write }: Late) => Effect.suspend(() => {
    trace.push("lands");
    return Effect.ignore(write);
  });
  const landLate = Effect.suspend(() => {
    for (let next = late.shift(); next !== undefined; next = late.shift()) {
      if (Date.now() < next.deadline) return Effect.as(land(next), true);
      trace.push("expired");
    }
    return Effect.succeed(false);
  });
  const doomed = (what: string, attempt: number, write: Effect.Effect<void, ClickHouseFailure>) => Effect.suspend(() => {
    const fate = pick(rng, fates);
    trace.push(`${what}:${fate}`);
    switch (fate) {
      case "ok": return write;
      case "lost": return write.pipe(Effect.andThen(Effect.fail(new ClickHouseFailure({ message: "acknowledgement lost", cause: null }))));
      case "late": late.push({ attempt, deadline: Date.now() + LATE_WINDOW_MS, write }); return Effect.fail(new ClickHouseFailure({ message: "timed out", cause: null }));
      case "cut": return Effect.forkDetach(write).pipe(Effect.andThen((fiber) => { daemons.push({ attempt, fiber }); return Effect.interrupt; }));
    }
  });
  const beforeCollecting = Effect.suspend(() => {
    if (gcFailures > 0) {
      gcFailures--;
      trace.push("gc-fails");
      return Effect.fail(new ClickHouseFailure({ message: "collection failed after fencing", cause: null }));
    }
    return late.length > 0 && rng() < 0.5 ? Effect.asVoid(landLate) : Effect.void;
  });
  const client: ClickHouseService = {
    ...real,
    unchecked: {
      ...real.unchecked,
      select: <A>(query: string, params?: Params): Effect.Effect<ReadonlyArray<A>, ClickHouseFailure> =>
        query.startsWith("SELECT count() AS n FROM") ? beforeCollecting.pipe(Effect.andThen(real.unchecked.select<A>(query, params))) : real.unchecked.select<A>(query, params),
      insert: (table, rows): Effect.Effect<void, ClickHouseFailure> =>
        rows.length === 0 ? real.unchecked.insert(table, rows) : doomed(`insert-${table}`, attemptOf(rows[0]), real.unchecked.insert(table, rows)),
      command: (query, params): Effect.Effect<void, ClickHouseFailure> => {
        const head = query.trim().split(/\s+/, 3).join(" ");
        if (head.startsWith("DELETE FROM")) return beforeCollecting.pipe(Effect.andThen(real.unchecked.command(query, params)));
        if (!head.startsWith("INSERT INTO")) return real.unchecked.command(query, params);
        const interleaved = head === "INSERT INTO daily" && late.length > 0 && rng() < 0.5 ? landLate : Effect.void;
        return interleaved.pipe(Effect.andThen(doomed(head.slice("INSERT INTO ".length).toLowerCase(), attemptOf(params), real.unchecked.command(query, params))));
      },
    },
  };
  const quiesce = Effect.gen(function* () {
    while (yield* landLate) {}
    yield* Effect.forEach(daemons, ({ fiber }) => Fiber.await(fiber), { discard: true });
  });
  return { client, landLate, quiesce, failCollections: (n: number) => { gcFailures = n; } };
};

const total = (rows: ReadonlyArray<{ pageviews: number }>) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0);
const dayOf = (bucket: Date) => Math.floor(bucket.getTime() / DAY);

const run = (seed: number, profile: Profile) => Effect.gen(function* () {
  forgetPartitions();
  const rng = mulberry32(seed);
  const trace: string[] = [];
  const { clickhouse: real } = yield* clickhouseTestDatabase;
  const clickhouse: ClickHouseService = { ...real, writeLifetimeSeconds: LIFETIME_SECONDS };
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const faulty = faults(clickhouse, rng, trace, profile.fates);
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
  const reads = (store: typeof clickHouseStore | typeof postgresStore, service = clickhouse) =>
    provide(Effect.all([store.dailyTotals(query), store.hourlyTotals(query), store.breakdown(query, "page")]), service);
  type Snapshot = Effect.Success<ReturnType<typeof reads>>;
  const noLoss = (label: string, later: Snapshot, earlier: Snapshot) =>
    later.forEach((component, i) => expect(total(component), label).toBeGreaterThanOrEqual(total(earlier[i] ?? [])));
  const noPhantom = (label: string, seen: Snapshot, truth: Snapshot) =>
    seen.forEach((component, i) => expect(total(component), label).toBeLessThanOrEqual(total(truth[i] ?? [])));
  const marker = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ dirty: string[]; demoted: boolean; pending: number; floor: string }>(sql`
      select dirty::text[] as dirty, backfilled_at is null as demoted, cardinality(pending) as pending, floor::text as floor
      from analytics.clickhouse_site where environment_id = ${site}::text`, "objects");
    return {
      dirty: new Set(row === undefined ? [] : row.dirty.map((day) => dayOf(new Date(`${day}T00:00:00Z`)))),
      demoted: row?.demoted ?? true,
      pending: row?.pending ?? 0,
      floor: Number(row?.floor ?? 0),
    };
  }));
  const enqueue = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(site, [row()])).returning());
  }));
  const step = (name: string, op: Effect.Effect<unknown>) => Effect.gen(function* () {
    trace.push(name);
    yield* Fiber.await(yield* Effect.forkChild(op));
  });
  const stall = Effect.gen(function* () {
    const captured = yield* Deferred.make<void>();
    const resume = yield* Deferred.make<void>();
    const gate = Deferred.succeed(captured, undefined).pipe(Effect.andThen(Deferred.await(resume)));
    const stalled: ClickHouseService = { ...clickhouse, unchecked: { ...clickhouse.unchecked, select: <A>(text: string, params?: Params) => gate.pipe(Effect.andThen(clickhouse.unchecked.select<A>(text, params))) } };
    return { captured, resume, stalled, since: snapshots.length - 1 };
  });
  type Paused = { readonly result: Fiber.Fiber<Snapshot>; readonly resume: Deferred.Deferred<void>; readonly since: number };
  type Escaped = { readonly result: Fiber.Fiber<ReadonlyArray<{ n: number }>, ClickHouseFailure>; readonly resume: Deferred.Deferred<void>; readonly since: number };
  const snapshots: Snapshot[] = [];
  const paused: Paused[] = [];
  const escaped: Escaped[] = [];
  const startRead = Effect.gen(function* () {
    const { captured, resume, stalled, since } = yield* stall;
    const result = yield* Effect.forkChild(reads(clickHouseStore, stalled));
    yield* Effect.raceFirst(Deferred.await(captured), Fiber.await(result));
    paused.push({ result, resume, since });
  });
  const finishRead = (label: string) => Effect.gen(function* () {
    const read = paused.shift();
    if (read === undefined) return;
    yield* Deferred.succeed(read.resume, undefined);
    const result = yield* Fiber.join(read.result);
    const seen = [...snapshots.slice(read.since), yield* reads(clickHouseStore)];
    const since = `${label}: a read that paused since snapshot ${read.since}`;
    const started = snapshots[read.since];
    if (started === undefined) return yield* Effect.die(`no snapshot ${read.since} for a paused read`);
    noLoss(since, result, started);
    result.forEach((component, i) => expect(seen.map((snapshot) => snapshot[i]), since).toContainEqual(component));
  });
  const startEscape = Effect.gen(function* () {
    const { captured, resume, stalled, since } = yield* stall;
    const result = yield* provide(read(stalled, site, (reader) => Effect.gen(function* () {
      const b = clickHouseStore.binder(query, reader.snapshot);
      const fiber = yield* Effect.forkDetach(reader.select<{ n: number }>(`SELECT count() AS n FROM pageview WHERE environment_id = ${b.env} AND ${b.visible}`, b.params));
      yield* Deferred.await(captured);
      return fiber;
    })));
    escaped.push({ result, resume, since });
  });
  const finishEscape = (label: string) => Effect.gen(function* () {
    const escape = escaped.shift();
    if (escape === undefined) return;
    yield* Deferred.succeed(escape.resume, undefined);
    const exit = yield* Fiber.await(escape.result);
    const started = snapshots[escape.since];
    if (started === undefined) return yield* Effect.die(`no snapshot ${escape.since} for an escaped read`);
    const since = `${label}: a Reader query that outlived its callback since snapshot ${escape.since}`;
    if (Exit.isSuccess(exit)) expect(Number(exit.value[0]?.n), since).toBeGreaterThanOrEqual(total(started[0]));
    else expect(String(exit.cause), since).toContain("A Reader answers only inside");
  });
  yield* provide(seedSite({ environmentId: site, timezone: "UTC" }, [row(), row()]));
  yield* provide(backfillSite(clickhouse, site));
  let fence: number | undefined;
  let previous = yield* reads(clickHouseStore);
  let before = yield* marker;
  snapshots.push(previous);
  for (let n = 0; n < profile.length; n++) {
    const kind = pick(rng, profile.steps);
    const label = () => `seed ${seed} after step ${n} (${kind}): ${trace.join(" ")}`;
    switch (kind) {
      case "add": yield* enqueue; yield* step(kind, provide(drainOutbox(faulty.client), faulty.client)); break;
      case "drain": yield* step(kind, provide(drainOutbox(faulty.client), faulty.client)); break;
      case "close": yield* step(kind, provide(closeDays(site, now), faulty.client)); break;
      case "rebuild": yield* step(kind, provide(rebuildDirtyDays(site), faulty.client)); break;
      case "backfill": yield* step(kind, provide(Effect.catchTag(backfillSite(faulty.client, site), "SiteUnavailable", () => Effect.succeed(0)), faulty.client)); break;
      case "pin": trace.push(kind); fence = yield* provide(pin()); break;
      case "collect": {
        const pinned = fence ?? (yield* provide(pin()));
        faulty.failCollections(Math.floor(rng() * 3));
        const retried = collectGarbage(faulty.client, pinned).pipe(Effect.tapError(() => Effect.sync(() => trace.push("retried"))), Effect.retry({ times: 5 }));
        yield* step(kind, provide(retried, faulty.client));
        break;
      }
      case "late": trace.push(kind); yield* faulty.landLate; break;
      case "settle": trace.push(kind); yield* Effect.sleep(SETTLE); break;
      case "read": trace.push(kind); yield* startRead; break;
      case "escape": trace.push(kind); yield* startEscape; break;
      case "finish": trace.push(kind); yield* finishRead(label()); yield* finishEscape(label()); break;
    }
    const current = yield* reads(clickHouseStore);
    const [daily, hourly] = current;
    const after = yield* marker;
    for (const day of daily) {
      if (after.dirty.has(dayOf(day.bucket))) continue;
      const hours = hourly.filter((hour) => dayOf(hour.bucket) === dayOf(day.bucket));
      expect({ pageviews: day.pageviews, visits: day.visits }, label()).toEqual({ pageviews: total(hours), visits: hours.reduce((sum, { visits }) => sum + visits, 0) });
    }
    noLoss(label(), current, previous);
    noPhantom(label(), current, yield* reads(postgresStore));
    if (kind === "collect") expect(current, label()).toEqual(previous);
    expect(after.floor, label()).toBeGreaterThanOrEqual(before.floor);
    expect(after.pending, label()).toBeLessThanOrEqual(PENDING_CAP);
    if (after.pending === PENDING_CAP) trace.push("capped");
    previous = current;
    before = after;
    snapshots.push(current);
  }
  if (profile.crossesCap) expect(trace, `seed ${seed} never filled the pending cap: ${trace.join(" ")}`).toContain("capped");
  const draining = `seed ${seed} draining paused reads: ${trace.join(" ")}`;
  while (paused.length > 0) yield* finishRead(draining);
  while (escaped.length > 0) yield* finishEscape(draining);
  yield* faulty.quiesce;
  yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
  yield* Effect.sleep(SETTLE);
  yield* provide(collectGarbage(clickhouse, yield* provide(pin())));
  yield* provide(ensureBackfilled(clickhouse, site));
  for (let n = 0; n < 10 && (yield* provide(drainOutbox(clickhouse))) > 0; n++) {}
  yield* provide(rebuildDirtyDays(site));
  yield* provide(closeDays(site, now));
  yield* provide(rebuildDirtyDays(site));
  expect(yield* reads(clickHouseStore), `seed ${seed} settled: ${trace.join(" ")}`).toEqual(yield* reads(postgresStore));
});

for (const profile of PROFILES) {
  for (const seed of profile.seeds) {
    it.live(`seed ${seed}: every read stays consistent while ClickHouse writes are lost, late, cut off, or collected by a failing collector`, () => run(seed, profile), 300_000);
  }
}
