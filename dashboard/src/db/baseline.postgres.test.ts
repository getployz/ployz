import { afterAll, beforeAll, expect, it } from "vitest";
import { orgCollections } from "#/collections/registry";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("creates collection tables with default replication identity", async () => {
  const tables = [...new Set(orgCollections.map((collection) => collection.sources[0]))].sort();
  const replication = await harness.pool.query<{
    relname: string;
    relreplident: string;
  }>(`
    select relname, relreplident
    from pg_class
    where relnamespace = 'public'::regnamespace and relname = any($1::text[])
    order by relname
  `, [tables]);
  expect(replication.rows.map((row) => row.relname)).toEqual(tables);
  expect(replication.rows.filter((row) => row.relreplident !== "d")).toEqual([]);
});
