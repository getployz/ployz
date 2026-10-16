import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, mint, PENDING_CAP, pin, PublishRefused } from "#/modules/web-analytics/clickhouse/attempt";
import { seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { Database } from "#/server/database.server";
import { silentClickHouse as silent } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const SITE = "00000000-0000-4000-8000-00000000a7e1";

type Site = { floor: number; below: number; pending: number[]; generation: number; reads: boolean; backfilled: boolean };

const setup = Effect.gen(function* () {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services), Effect.orDie);
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, []));
  // The site row is created by the first mint, so before it nothing is pending.
  const site = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ floor: string; below: string; pending: string[]; generation: string; reads: boolean; backfilled: boolean }>(sql`
      select floor::text as floor, below::text as below, pending::text[] as pending, generation::text as generation, reads, backfilled_at is not null as backfilled
      from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
    if (row === undefined) return null;
    return { floor: Number(row.floor), below: Number(row.below), pending: row.pending.map(Number), generation: Number(row.generation), reads: row.reads, backfilled: row.backfilled } satisfies Site;
  }));
  const pending = Effect.map(site, (row) => row?.pending ?? []);
  const minted = Effect.gen(function* () {
    const writer = (yield* mint(silent, [SITE])).get(SITE);
    if (writer === undefined) return yield* Effect.die("mint gave the seeded site no attempt");
    return writer;
  });
  const database = yield* provide(Database);
  const defect = <A, E, R>(effect: Effect.Effect<A, E, R>) => provide(Effect.catchDefect(effect, (cause) => Effect.fail(String(cause))).pipe(Effect.flip, Effect.map(String)));
  return { provide, site, pending, minted, database, defect };
});

it.live("mint lists the attempt as pending before any row carries it, and gives a site that does not exist nothing", () =>
  Effect.gen(function* () {
    const { provide, pending, minted } = yield* setup;
    expect(yield* pending).toEqual([]);
    const writer = yield* provide(minted);
    expect(yield* pending).toEqual([writer.attempt]);
    expect(yield* provide(mint(silent, ["00000000-0000-4000-8000-00000000a7e2"]))).toEqual(new Map());
  }));

it.live("an attempt publishes only inside a transaction, only once, and accepts no rows afterwards", () =>
  Effect.gen(function* () {
    const { provide, pending, minted, database, defect } = yield* setup;
    const writer = yield* provide(minted);
    yield* provide(writer.rows("pageview", [{}]));
    expect(yield* defect(writer.publish({ kind: "append", dirty: [] }))).toContain("publish runs inside the transaction");
    expect(yield* pending).toEqual([writer.attempt]);
    yield* provide(database.transaction(writer.publish({ kind: "append", dirty: [] })));
    expect(yield* pending).toEqual([]);
    expect(yield* defect(writer.rows("pageview", [{}]))).toContain("already published");
    expect(yield* defect(database.transaction(writer.publish({ kind: "append", dirty: [] })))).toContain("cannot publish twice");
  }));

it.live("an attempt minted before a garbage collection fence can never publish, one minted after it still can, and every collection moves the generation", () =>
  Effect.gen(function* () {
    const { provide, site, pending, minted, database } = yield* setup;
    const stale = yield* provide(minted);
    const fence = yield* provide(pin());
    const fresh = yield* provide(minted);
    const before = yield* site;
    yield* provide(collectGarbage(silent, fence));
    expect((yield* site)?.generation).toBe((before?.generation ?? 0) + 1);
    expect(yield* pending).toEqual([stale.attempt, fresh.attempt]);
    const refused = yield* provide(database.transaction(Effect.flip(stale.publish({ kind: "append", dirty: [] }))));
    expect(refused).toBeInstanceOf(PublishRefused);
    yield* provide(database.transaction(fresh.publish({ kind: "append", dirty: [] })));
    expect(yield* pending).toEqual([stale.attempt]);
  }));

it.live("a site holding the pending cap of lost attempts forgets them on the next mint and demotes until a backfill republishes it", () =>
  Effect.gen(function* () {
    const { provide, site, minted, database } = yield* setup;
    const { drizzle } = database;
    const [cap] = yield* provide(drizzle.execute<{ definition: string }>(sql`
      select pg_get_constraintdef(oid) as definition from pg_constraint where conname = 'clickhouse_site_pending_cap_check'`, "objects"));
    expect(cap?.definition).toBe(`CHECK ((cardinality(pending) <= ${PENDING_CAP}))`);
    const lost: number[] = [];
    for (let i = 0; i < PENDING_CAP; i++) lost.push((yield* provide(minted)).attempt);
    yield* provide(drizzle.execute(sql`update analytics.clickhouse_site set reads = true, backfilled_at = now() where environment_id = ${SITE}::text`));
    expect((yield* site)?.pending).toEqual(lost);
    const next = yield* provide(minted);
    const past = (lost.at(-1) ?? 0) + 1;
    expect(yield* site).toEqual({ floor: past, below: past, pending: [next.attempt], generation: 0, reads: false, backfilled: false });
    yield* provide(database.transaction(next.publish({ kind: "append", dirty: [] })));
    expect((yield* site)?.pending).toEqual([]);
  }));
