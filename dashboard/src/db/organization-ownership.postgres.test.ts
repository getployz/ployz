import { afterAll, beforeAll, expect, it } from "vitest";
import { changeNameSources } from "#/collections/change-sources";
import { changeSources } from "#/modules/organization/change-log.sources";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";

/**
 * Every row of an organization-owned table belongs to exactly one Organization,
 * directly or through its parent row, and stores it in this column. The change log
 * registry is the one list of organization-owned tables.
 */
const organizationOwned = Object.fromEntries(Object.entries(changeSources).map(([table, source]) =>
  [table, "organizationColumn" in source ? source.organizationColumn : "organization_id"]));

const notOrganizationOwned = {
  user: "Belongs to a user.",
  account: "Belongs to a user.",
  verification: "Belongs to a user.",
  session: "Belongs to a user; its active Organization doesn't make it organization-owned.",
  github_installation: "Belongs to a user.",
  github_repository_cache: "Belongs to a user.",
  github_branch_projection: "Belongs to a GitHub installation, which several Organizations can share.",
  github_check_suite_projection: "Belongs to a GitHub installation, which several Organizations can share.",
  github_webhook_delivery: "Belongs to a GitHub installation, which several Organizations can share.",
  waitlist: "Belongs to no one.",
  organization_change: "It is the Organization change log, written by the triggers on organization-owned tables.",
} satisfies Record<string, string>;

let harness: GithubPostgresTestHarness;

beforeAll(async () => {
  harness = await startGithubPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("classifies every table as organization-owned or excluded with a reason", async () => {
  const tables = await harness.pool.query<{ relname: string }>(`
    select relname from pg_class
    where relnamespace = 'public'::regnamespace and relkind in ('r', 'p')
    order by relname
  `);
  const classified = [...Object.keys(organizationOwned), ...Object.keys(notOrganizationOwned)];
  expect(new Set(classified).size).toBe(classified.length);
  expect(tables.rows.map((row) => row.relname)).toEqual(classified.sort());
});

it("stores a non-null Organization on every organization-owned table", async () => {
  const columns = await harness.pool.query<{ table_name: string; column_name: string }>(`
    select table_name, column_name from information_schema.columns
    where table_schema = 'public' and is_nullable = 'NO'
      and (table_name, column_name) in (select * from unnest($1::text[], $2::text[]))
  `, [Object.keys(organizationOwned), Object.values(organizationOwned)]);
  expect(Object.fromEntries(columns.rows.map((row) => [row.table_name, row.column_name])))
    .toEqual(organizationOwned);
});

it("logs every change to an organization-owned table under its Organization and the key its collections share", async () => {
  const triggers = await harness.pool.query<{ table_name: string; event: string; tgargs: Buffer }>(`
    select c.relname as table_name, t.tgname as event, t.tgargs
    from pg_trigger t join pg_class c on c.oid = t.tgrelid join pg_proc p on p.oid = t.tgfoid
    where p.proname = 'organization_change_log'
  `);
  const row = (table: string, event: string, args: string[]) => `${table} ${event}(${args.join(", ")})`;
  // Arguments are the Organization column, then the one key every collection fed by the table shares.
  const expected = Object.entries(organizationOwned).flatMap(([table, organizationColumn]) =>
    ["insert", "update", "delete"].map((event) =>
      row(table, `organization_change_${event}`, [organizationColumn, ...new Map(Object.entries(changeSources)).get(table)?.key ?? []])));
  expect(triggers.rows.map((trigger) =>
    row(trigger.table_name, trigger.event, trigger.tgargs.toString("utf8").split("\0").filter(Boolean))).sort())
    .toEqual(expected.sort());
});

it("keys every source feeding a collection by its key table's key", async () => {
  const foreignKeys = await harness.pool.query<{ source: string; columns: string[]; target: string; target_columns: string[] }>(`
    select c.conrelid::regclass::text as source, c.confrelid::regclass::text as target,
      array(select a.attname::text from unnest(c.conkey) with ordinality k(n, i)
        join pg_attribute a on a.attrelid = c.conrelid and a.attnum = k.n order by k.i) as columns,
      array(select a.attname::text from unnest(c.confkey) with ordinality k(n, i)
        join pg_attribute a on a.attrelid = c.confrelid and a.attnum = k.n order by k.i) as target_columns
    from pg_constraint c where c.contype = 'f'
  `);
  const references = new Set(foreignKeys.rows.map((row) => `${row.source}(${row.columns.join(", ")}) -> ${row.target}(${row.target_columns.join(", ")})`));
  // A change to any source names the collection rows it affects only if it logs their key.
  const required = Object.values(changeNameSources).flatMap(([keyTable, ...others]) =>
    others.map((source) => `${source}(${changeSources[source].key.join(", ")}) -> ${keyTable}(${changeSources[keyTable].key.join(", ")})`));
  expect(required.length).toBeGreaterThan(0);
  expect(required.filter((reference) => !references.has(reference))).toEqual([]);
});
