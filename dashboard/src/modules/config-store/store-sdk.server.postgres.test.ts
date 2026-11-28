import { Effect } from "effect";
import { afterAll, beforeAll, expect, it } from "vitest";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("migrates once when db:migrate and the worker open a new Store at once, and a later open finds nothing to do", async () => {
  const open = () => Effect.runPromise(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  await Promise.all([open(), open()]);
  const migrations = async () => (await harness.pool.query<{ name: string }>("select name from config_migration order by name")).rows;
  const applied = await migrations();
  expect(applied.length).toBeGreaterThan(0);
  expect(new Set(applied.map(({ name }) => name)).size).toBe(applied.length);
  await open();
  expect(await migrations()).toEqual(applied);
}, 60_000);
