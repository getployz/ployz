import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, insertRows, mint, pin, publish, PublishRefused } from "#/modules/web-analytics/clickhouse/attempt";

import { seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { Database } from "#/server/database.server";
import { silentClickHouse as silent } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const SITE = "00000000-0000-4000-8000-00000000a7e1";

const setup = Effect.gen(function* () {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services), Effect.orDie);
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, []));
  // The site row is created by the first mint, so before it nothing is pending.
  const pending = provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ pending: string[] }>(sql`
      select pending::text[] as pending from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
    return row === undefined ? [] : row.pending.map(Number);
  }));
  const minted = Effect.gen(function* () {
    const attempt = (yield* mint([SITE])).get(SITE);
    if (attempt === undefined) return yield* Effect.die("mint gave the seeded site no attempt");
    return attempt;
  });
  const advance = sql`below = greatest(below, 1)`;
  return { provide, pending, minted, advance };
});

it.live("mint lists the attempt as pending before any row carries it, and gives a site that does not exist nothing", () =>
  Effect.gen(function* () {
    const { provide, pending, minted } = yield* setup;
    expect(yield* pending).toEqual([]);
    const attempt = yield* provide(minted);
    expect(yield* pending).toEqual([attempt]);
    expect(yield* provide(mint(["00000000-0000-4000-8000-00000000a7e2"]))).toEqual(new Map());
  }));

it.live("publish commits only inside a transaction and only once per attempt", () =>
  Effect.gen(function* () {
    const { provide, pending, minted, advance } = yield* setup;
    const attempt = yield* provide(minted);
    const proof = yield* provide(insertRows(silent, "pageview", [], attempt));
    const outside = yield* provide(Effect.catchDefect(publish(proof, SITE, advance), (cause) => Effect.fail(String(cause))).pipe(Effect.flip));
    expect(outside).toContain("publish runs inside the transaction");
    expect(yield* pending).toEqual([attempt]);
    const database = yield* provide(Database);
    yield* provide(database.transaction(publish(proof, SITE, advance)));
    expect(yield* pending).toEqual([]);
    const again = yield* provide(database.transaction(Effect.flip(publish(proof, SITE, advance))));
    expect(again).toBeInstanceOf(PublishRefused);
  }));

it.live("an attempt minted before a garbage collection fence can never publish, and one minted after it still can", () =>
  Effect.gen(function* () {
    const { provide, pending, minted, advance } = yield* setup;
    const stale = yield* provide(minted);
    const fence = yield* provide(pin());
    const fresh = yield* provide(minted);
    yield* provide(collectGarbage(silent, fence));
    expect(yield* pending).toEqual([stale, fresh]);
    const database = yield* provide(Database);
    const refused = yield* provide(database.transaction(Effect.flip(publish(yield* provide(insertRows(silent, "pageview", [], stale)), SITE, advance))));
    expect(refused).toBeInstanceOf(PublishRefused);
    yield* provide(database.transaction(publish(yield* provide(insertRows(silent, "pageview", [], fresh)), SITE, advance)));
    expect(yield* pending).toEqual([stale]);
  }));
