import { it } from "@effect/vitest";
import { and, asc, eq, sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { owners } from "#/modules/config-store/store-lookup.server";
import { ackSalts, saltWindow, setSiteAnalytics, siteDay, withholdToday } from "#/modules/web-analytics/salts.server";
import { analyticsBotDaily, analyticsDaily, analyticsGapDaily, analyticsGoal, analyticsRollupState, analyticsSalt, analyticsSessionHourly, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f401";
const PROJECT = "00000000-0000-4000-8000-00000000f402";
const PRODUCTION = "00000000-0000-4000-8000-00000000f403";
const STAGING = "00000000-0000-4000-8000-00000000f404";

it("a Sydney day is 23 hours on the first Sunday of October and starts at its own midnight", () => {
  expect(saltWindow("2026-10-03", "Australia/Sydney")).toEqual({ start: Date.UTC(2026, 9, 2, 14) / 1000, end: Date.UTC(2026, 9, 3, 14) / 1000 });
  const shift = saltWindow("2026-10-04", "Australia/Sydney");
  expect(shift).toEqual({ start: Date.UTC(2026, 9, 3, 14) / 1000, end: Date.UTC(2026, 9, 4, 13) / 1000 });
  expect(shift.end - shift.start).toBe(23 * 3600);
  expect(saltWindow("2026-10-05", "Australia/Sydney").start).toBe(shift.end);
});

it("a New York day is 23 hours when clocks spring forward and 25 when they fall back", () => {
  const spring = saltWindow("2026-03-08", "America/New_York");
  expect(spring).toEqual({ start: Date.UTC(2026, 2, 8, 5) / 1000, end: Date.UTC(2026, 2, 9, 4) / 1000 });
  expect(spring.end - spring.start).toBe(23 * 3600);
  const fall = saltWindow("2026-11-01", "America/New_York");
  expect(fall).toEqual({ start: Date.UTC(2026, 10, 1, 4) / 1000, end: Date.UTC(2026, 10, 2, 5) / 1000 });
  expect(fall.end - fall.start).toBe(25 * 3600);
});

it("a site's day is its own calendar day, not UTC's", () => {
  const at = Date.UTC(2026, 9, 15, 20);
  expect(siteDay("UTC", at)).toBe("2026-10-15");
  expect(siteDay("Australia/Sydney", at)).toBe("2026-10-16");
  expect(siteDay("Australia/Sydney", at, -1)).toBe("2026-10-15");
  expect(siteDay("America/New_York", Date.UTC(2026, 11, 31, 12), 2)).toBe("2027-01-02");
});

const setup = Effect.fn(function* (env: Readonly<Record<string, string>> = { ANALYTICS_ENABLED: "true" }) {
  const services = yield* Layer.build(yield* storeTestCloud({ env }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  const store = yield* provide(cloudStore);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: PRODUCTION }));
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_environment", id: STAGING, project: "shop", name: "staging" }));
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      insert into config_namespace (organization_id, namespace, environment_id)
      values (${ORGANIZATION}, 'shop-prod', ${PRODUCTION}), (${ORGANIZATION}, 'shop-staging', ${STAGING})
      on conflict do nothing`);
  }).pipe(Effect.orDie));
  const owned = yield* provide(owners(ORGANIZATION));
  const namespace = (environment: string) => owned.find((candidate) => candidate.environment === environment)?.namespace;
  const days = (environmentId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ day: analyticsSalt.day, salt: analyticsSalt.salt }).from(analyticsSalt)
      .where(and(eq(analyticsSalt.organizationId, ORGANIZATION), eq(analyticsSalt.environmentId, environmentId)))
      .orderBy(asc(analyticsSalt.day));
  }).pipe(Effect.orDie));
  return { provide, namespace, days };
});

it.live("turning a site on issues today and the next two days, and only on sites reach the ack", () => Effect.gen(function* () {
  const { provide, namespace, days } = yield* setup();
  expect(yield* provide(ackSalts(ORGANIZATION))).toEqual({ sites: [] });
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "Australia/Sydney" }));
  yield* provide(setSiteAnalytics(ORGANIZATION, STAGING, { enabled: false }));
  const now = Date.now();
  const issued = [0, 1, 2].map((offset) => siteDay("Australia/Sydney", now, offset));
  expect((yield* days(PRODUCTION)).map(({ day }) => day)).toEqual(issued);
  expect(yield* days(STAGING)).toEqual([]);

  const ack = yield* provide(ackSalts(ORGANIZATION));
  expect(namespace("production")).toBeDefined();
  expect(ack.sites.map((site) => site.namespace)).toEqual([namespace("production")]);
  expect(ack.sites[0]?.salts.map(({ day, start, end }) => ({ day, start, end })))
    .toEqual(issued.map((day) => ({ day, ...saltWindow(day, "Australia/Sydney") })));
  for (const { salt } of ack.sites[0]?.salts ?? []) {
    expect(salt).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(Buffer.from(salt, "base64url")).toHaveLength(32);
  }

  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: false }));
  expect(yield* provide(ackSalts(ORGANIZATION))).toEqual({ sites: [] });
  expect(yield* days(PRODUCTION)).toHaveLength(3);
}), 60_000);

it.live("an ack never recreates a withheld today, and keeps the salts it already issued", () => Effect.gen(function* () {
  const { provide, days } = yield* setup();
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true }));
  const before = yield* days(PRODUCTION);
  yield* provide(withholdToday(ORGANIZATION, PRODUCTION));
  const ack = yield* provide(ackSalts(ORGANIZATION));
  yield* provide(ackSalts(ORGANIZATION));
  const after = yield* days(PRODUCTION);
  expect(after).toEqual(before.slice(1));
  expect(ack.sites[0]?.salts.map(({ day, salt }) => ({ day, salt })))
    .toEqual(after.map(({ day, salt }) => ({ day, salt: Buffer.from(salt).toString("base64url") })));
}), 60_000);

it.live("an ack adds the next two days for an on site, and carries yesterday's salt but not older ones", () => Effect.gen(function* () {
  const { provide, days } = yield* setup();
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "America/New_York" }));
  const now = Date.now();
  const day = (offset: number) => siteDay("America/New_York", now, offset);
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.delete(analyticsSalt).where(eq(analyticsSalt.day, day(2)));
    yield* drizzle.insert(analyticsSalt).values([-2, -1].map((offset) => ({
      organizationId: ORGANIZATION, environmentId: PRODUCTION, day: day(offset), salt: Buffer.alloc(32, offset + 10),
    })));
  }).pipe(Effect.orDie));
  const ack = yield* provide(ackSalts(ORGANIZATION));
  expect((yield* days(PRODUCTION)).map((row) => row.day)).toEqual([-2, -1, 0, 1, 2].map(day));
  expect(ack.sites[0]?.salts.map((salt) => salt.day)).toEqual([-1, 0, 1, 2].map(day));
}), 60_000);

it.live("an unknown time zone is refused", () => Effect.gen(function* () {
  const { provide } = yield* setup();
  const refused = yield* provide(Effect.flip(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "Mars/Olympus" })));
  expect(refused._tag).toBe("Validation");
}), 60_000);

it.live("a Self-hosted Cloud without ANALYTICS_ENABLED acks no salts, even for an on site", () => Effect.gen(function* () {
  const { provide, days } = yield* setup({});
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true }));
  expect(yield* days(PRODUCTION)).toHaveLength(3);
  expect(yield* provide(ackSalts(ORGANIZATION))).toEqual({ sites: [] });
}), 60_000);

it.live("turning a site on starts its rollup state empty, and turning it off or on again keeps it", () => Effect.gen(function* () {
  const { provide } = yield* setup();
  const rollupState = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ environmentId: analyticsRollupState.environmentId, rolledUpTo: analyticsRollupState.rolledUpTo, dirty: analyticsRollupState.dirty })
      .from(analyticsRollupState);
  }).pipe(Effect.orDie));
  yield* provide(setSiteAnalytics(ORGANIZATION, STAGING, { enabled: false }));
  expect(yield* rollupState()).toEqual([]);
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true }));
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: null, dirty: [] }]);
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(analyticsRollupState).set({ rolledUpTo: "2026-10-01", dirty: ["2026-09-30"] });
  }).pipe(Effect.orDie));
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: false }));
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true }));
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: "2026-10-01", dirty: ["2026-09-30"] }]);
}), 60_000);

it.live("a new time zone drops the site's rollups and starts its rollup state over; the same one keeps them", () => Effect.gen(function* () {
  const { provide } = yield* setup();
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "Australia/Sydney" }));
  yield* provide(setSiteAnalytics(ORGANIZATION, STAGING, { enabled: true, timezone: "Australia/Sydney" }));
  const totals = { pageviews: 3, visitors: 2, visits: 2, bounces: 1, totalDuration: 60 };
  const session = { visits: 2, bounces: 1, totalDuration: 60 };
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    for (const environmentId of [PRODUCTION, STAGING]) {
      yield* drizzle.insert(analyticsDaily).values({ environmentId, day: "2026-10-01", dim: "page", value: "/", ...totals });
      yield* drizzle.insert(analyticsSessionHourly).values({ environmentId, hour: new Date("2026-10-01T00:00:00Z"), pageviews: 3, visitors: 1, ...session });
    }
    yield* drizzle.update(analyticsRollupState).set({ rolledUpTo: "2026-10-01", dirty: ["2026-09-30"] });
  }).pipe(Effect.orDie));
  const rollups = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return {
      daily: (yield* drizzle.select({ environmentId: analyticsDaily.environmentId }).from(analyticsDaily).orderBy(asc(analyticsDaily.environmentId)))
        .map(({ environmentId }) => environmentId),
      hourly: (yield* drizzle.select({ environmentId: analyticsSessionHourly.environmentId }).from(analyticsSessionHourly)
        .orderBy(asc(analyticsSessionHourly.environmentId))).map(({ environmentId }) => environmentId),
      state: yield* drizzle.select({ environmentId: analyticsRollupState.environmentId, rolledUpTo: analyticsRollupState.rolledUpTo, dirty: analyticsRollupState.dirty })
        .from(analyticsRollupState).orderBy(asc(analyticsRollupState.environmentId)),
    };
  }).pipe(Effect.orDie));
  const untouched = yield* rollups();
  expect(untouched).toEqual({
    daily: [PRODUCTION, STAGING],
    hourly: [PRODUCTION, STAGING],
    state: [PRODUCTION, STAGING].map((environmentId) => ({ environmentId, rolledUpTo: "2026-10-01", dirty: ["2026-09-30"] })),
  });

  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "Australia/Sydney" }));
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: false }));
  expect(yield* rollups()).toEqual(untouched);

  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "America/New_York" }));
  expect(yield* rollups()).toEqual({
    daily: [STAGING],
    hourly: [STAGING],
    state: [
      { environmentId: PRODUCTION, rolledUpTo: null, dirty: [] },
      { environmentId: STAGING, rolledUpTo: "2026-10-01", dirty: ["2026-09-30"] },
    ],
  });
}), 60_000);

it.live("deleting an environment deletes its analytics and keeps every other environment's", () => Effect.gen(function* () {
  const { provide } = yield* setup();
  for (const environmentId of [PRODUCTION, STAGING]) yield* provide(setSiteAnalytics(ORGANIZATION, environmentId, { enabled: true }));
  const tables = [analyticsSite, analyticsSalt, analyticsRollupState, analyticsDaily, analyticsSessionHourly, analyticsBotDaily, analyticsGapDaily, analyticsGoal];
  const left = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const environments = [];
    for (const table of tables) {
      const rows = yield* drizzle.selectDistinct({ environmentId: table.environmentId }).from(table).orderBy(asc(table.environmentId));
      environments.push(rows.map(({ environmentId }) => environmentId));
    }
    return environments;
  }).pipe(Effect.orDie));
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    for (const environmentId of [PRODUCTION, STAGING]) {
      yield* drizzle.insert(analyticsDaily).values({ environmentId, day: "2026-10-01", dim: "", value: "", pageviews: 1, visitors: 1, visits: 1, bounces: 1, totalDuration: 0 });
      yield* drizzle.insert(analyticsSessionHourly).values({ environmentId, hour: new Date("2026-10-01T00:00:00Z"), pageviews: 1, visitors: 1, visits: 1, bounces: 1, totalDuration: 0 });
      yield* drizzle.insert(analyticsBotDaily).values({ environmentId, day: "2026-10-01", reason: "ua", count: 1 });
      yield* drizzle.insert(analyticsGapDaily).values({ environmentId, day: "2026-10-01", count: 1 });
      yield* drizzle.insert(analyticsGoal).values({ environmentId, name: "Signup", pathPattern: "/signup" });
    }
  }).pipe(Effect.orDie));
  expect(yield* left()).toEqual(tables.map(() => [PRODUCTION, STAGING]));

  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`delete from config_environment where id = ${STAGING}`);
  }).pipe(Effect.orDie));
  expect(yield* left()).toEqual(tables.map(() => [PRODUCTION]));
}), 60_000);
