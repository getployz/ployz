import { afterAll, beforeAll, expect, it } from "vitest";
import { collectionReadInput } from "#/collections/read.contract";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";

let harness: GithubPostgresTestHarness;

beforeAll(async () => {
  harness = await startGithubPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("creates collection tables with default replication identity without the retired notification triggers", async () => {
  const tables = [...collectionReadInput.fields.table.literals].sort();
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

  const triggers = await harness.pool.query(`
    select tgname from pg_trigger
    where not tgisinternal and tgname like 'ployz_%_realtime_changed'
  `);
  expect(triggers.rows).toEqual([]);
});
