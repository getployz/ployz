import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { afterAll, beforeAll, expect, it } from "vitest";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { readSealedVariables } from "#/modules/sealing/sealed-variables.server";
import { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const sealed = "00000000-0000-4000-8000-000000000e01";
const dark = "00000000-0000-4000-8000-000000000e02";

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("is off before the Store has made its table, and leaves the caller's transaction usable", async () => {
  const inTransaction = Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const on = yield* readSealedVariables(sealed);
      const { drizzle } = yield* Database;
      const [after] = yield* drizzle.execute<{ ok: number }>(sql`select 1 as ok`, "objects");
      return { on, after };
    }));
  });

  expect(await harness.runEffect(inTransaction)).toEqual({ on: false, after: { ok: 1 } });
});

it("is on only for an Organization with its own row", async () => {
  await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  await harness.pool.query("insert into config_sealed_variables (organization_id) values ($1)", [sealed]);

  expect(await harness.runEffect(readSealedVariables(sealed))).toBe(true);
  expect(await harness.runEffect(readSealedVariables(dark))).toBe(false);
});
