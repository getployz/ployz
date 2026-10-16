import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { expect } from "vitest";
import { type Acknowledged, type Attempt, collectGarbage, insertRows, insertSelect, mint, publish } from "#/modules/web-analytics/clickhouse/attempt";
import type { ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { Database, type DatabaseService } from "#/server/database.server";

declare const attempt: Attempt;
declare const proof: Acknowledged;

const client: ClickHouseService = {
  query: () => Effect.succeed([]),
  unchecked: { command: () => Effect.void, insert: () => Effect.void },
};

it("a write needs an Attempt from mint and a publication needs the proof an insert returns", () => {
  const rejected = [
    // @ts-expect-error a number is not an Attempt
    () => insertRows(client, "pageview", [], 7),
    // @ts-expect-error a number is not an Attempt
    () => insertSelect(client, 7, []),
    // @ts-expect-error an Attempt is not proof that its rows were acknowledged
    () => publish(attempt, "site", sql``),
    // @ts-expect-error an object carrying an attempt is not proof either
    () => publish({ attempt }, "site", sql``),
  ];
  const accepted = () => publish(proof, "site", sql``);
  expect(rejected).toHaveLength(4);
  expect(accepted).toBeTypeOf("function");
});

// SAFETY: the calls under test die on `inTransaction` before touching drizzle.
const untouched = {} as DatabaseService["drizzle"];
const insideTransaction: DatabaseService = {
  drizzle: untouched,
  inTransaction: true,
  afterCommit: () => Effect.void,
  transaction: (program) => Effect.die(`transaction must not run: ${String(program)}`),
};

const defect = <A, E>(effect: Effect.Effect<A, E, Database>) =>
  effect.pipe(Effect.catchDefect((cause) => Effect.fail(String(cause))), Effect.flip, Effect.map(String), Effect.provideService(Database, insideTransaction));

it.effect("mint refuses to run inside a caller's transaction, so an attempt is pending before any row carries it", () =>
  Effect.gen(function* () {
    expect(yield* defect(mint(["site"]))).toContain("mint commits on its own");
  }));

it.effect("garbage collection refuses to run inside a caller's transaction, so its fence commits before it deletes", () =>
  Effect.gen(function* () {
    expect(yield* defect(collectGarbage(client, 1))).toContain("collectGarbage commits on its own");
  }));
