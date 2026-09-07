import { execFile as execFileCallback } from "node:child_process";
import { promisify } from "node:util";
import { afterAll, beforeAll, expect, it } from "vitest";
import { PLOYZ_TABLES } from "#/electric/synced-tables.server";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";

const execFile = promisify(execFileCallback);
let harness: GithubPostgresTestHarness;

beforeAll(async () => {
  harness = await startGithubPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("creates Electric-ready tables without the retired notification triggers", async () => {
  const tables = Object.keys(PLOYZ_TABLES).sort();
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
  expect(replication.rows.filter((row) => row.relreplident !== "f")).toEqual([]);

  const triggers = await harness.pool.query(`
    select tgname from pg_trigger
    where not tgisinternal and tgname like 'ployz_%_realtime_changed'
  `);
  expect(triggers.rows).toEqual([]);
});

it("reruns migrations without replaying the baseline or losing application rows", async () => {
  await harness.pool.query(`
    insert into organization (name, slug) values ('Baseline', 'baseline')
  `);
  const before = await harness.pool.query("select * from drizzle.__drizzle_migrations");
  await execFile(process.execPath, ["node_modules/drizzle-kit/bin.cjs", "migrate"], {
    cwd: process.cwd(),
    env: { ...process.env, DATABASE_URL: harness.databaseUrl },
  });
  const after = await harness.pool.query("select * from drizzle.__drizzle_migrations");
  expect(after.rows).toEqual(before.rows);
  expect(await harness.pool.query("select name from organization where slug = 'baseline'"))
    .toMatchObject({ rows: [{ name: "Baseline" }] });
});
