import { randomUUID } from "node:crypto";
import { execFile as execFileCallback } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { Client } from "pg";
import { inject } from "vitest";
import { describe, expect, test } from "vitest";

const execFile = promisify(execFileCallback);
const migration = "20261014065748_org_github_links";
const migrations = resolve("drizzle");

async function migrate(databaseUrl: string, folder: string) {
  const cache = resolve("node_modules/.cache");
  mkdirSync(cache, { recursive: true });
  const config = join(mkdtempSync(join(cache, "ployz-migration-config-")), "drizzle.config.ts");
  writeFileSync(config, [
    'import { defineConfig } from "drizzle-kit";',
    `export default defineConfig({ dialect: "postgresql", schema: "${resolve("src/db/schema.ts")}", out: "${folder}", dbCredentials: { url: "${databaseUrl}" } });`,
  ].join("\n"));
  await execFile(process.execPath, ["node_modules/drizzle-kit/bin.cjs", "migrate", "--config", config], {
    cwd: process.cwd(),
    maxBuffer: 20 * 1024 * 1024,
  });
}

function migrationsBefore(name: string) {
  const folder = mkdtempSync(join(tmpdir(), "ployz-migrations-before-"));
  for (const entry of readdirSync(migrations)) {
    if (entry < name) symlinkSync(join(migrations, entry), join(folder, entry));
  }
  return folder;
}

function migrationStatements() {
  const sql = readFileSync(join(migrations, migration, "migration.sql"), "utf8");
  const statements = sql.split("--> statement-breakpoint");
  const find = (text: string) => {
    const statement = statements.find((part) => part.includes(text));
    if (!statement) throw new Error(`the migration lost ${text}`);
    return statement;
  };
  return { createLinks: find('CREATE TABLE "organization_github_installation"'), backfill: find('INSERT INTO "organization_github_installation"') };
}

async function backfillTwice(client: Client) {
  const { createLinks, backfill } = migrationStatements();
  await client.query(createLinks);
  await client.query(backfill);
  const once = await client.query(`select count(*)::int as count from organization_github_installation`);
  await client.query(backfill);
  const twice = await client.query(`select count(*)::int as count from organization_github_installation`);
  await client.query(`drop table organization_github_installation`);
  return { once: once.rows[0].count, twice: twice.rows[0].count };
}

async function withClient<A>(url: string, use: (client: Client) => Promise<A>) {
  const client = new Client({ connectionString: url });
  await client.connect();
  try {
    return await use(client);
  } finally {
    await client.end();
  }
}

async function emptyDatabase() {
  const name = `migration_${randomUUID().replaceAll("-", "")}`;
  const admin = inject("postgresAdminUrl");
  await withClient(admin, (client) => client.query(`create database ${name}`));
  const url = new URL(admin);
  url.pathname = `/${name}`;
  return {
    url: url.href,
    drop: () => withClient(admin, (client) => client.query(`drop database if exists ${name} with (force)`)),
  };
}

const ada = randomUUID();
const bob = randomUUID();
const adaShop = randomUUID();
const adaLab = randomUUID();
const hopper = randomUUID();
const bobOwn = randomUUID();
const adminOnly = randomUUID();

async function seedUserInstallations(client: Client) {
  await client.query(
    `insert into "user" (id, name, email, email_verified) values ($1, 'Ada', 'ada@example.com', true), ($2, 'Bob', 'bob@example.com', true)`,
    [ada, bob],
  );
  await client.query(
    `insert into organization (id, name, slug) values ($1, 'Ada Shop', 'ada-shop'), ($2, 'Ada Lab', 'ada-lab'), ($3, 'Hopper', 'hopper'), ($4, 'Bob Own', 'bob-own'), ($5, 'Admin Only', 'admin-only')`,
    [adaShop, adaLab, hopper, bobOwn, adminOnly],
  );
  await client.query(
    `insert into member (id, user_id, organization_id, role) values
      ($1, $5, $7, 'owner'), ($2, $5, $8, 'owner'), ($3, $5, $9, 'admin'), ($4, $6, $10, 'owner'), ($11, $6, $9, 'owner'), ($12, $5, $13, 'admin')`,
    [randomUUID(), randomUUID(), randomUUID(), randomUUID(), ada, bob, adaShop, adaLab, hopper, bobOwn, randomUUID(), randomUUID(), adminOnly],
  );
  await client.query(
    `insert into github_installation (user_id, installation_id, account_login, account_type, created_at) values
      ($1, 7, 'acme', 'Organization', '2026-01-01T00:00:00Z'),
      ($2, 7, 'acme', 'Organization', '2026-02-01T00:00:00Z'),
      ($1, 8, 'ada', 'User', '2026-03-01T00:00:00Z')`,
    [ada, bob],
  );
  await client.query(
    `insert into github_repository_cache (user_id, installation_id, repository_id, name, full_name, default_branch, private, html_url, repo_updated_at, synced_at) values
      ($1, 7, 4242, 'blog', 'acme/blog', 'main', false, 'https://github.com/acme/blog', now(), '2026-02-01T00:00:00Z'),
      ($2, 7, 4242, 'blog', 'acme/blog', 'trunk', false, 'https://github.com/acme/blog', now(), '2026-03-01T00:00:00Z'),
      ($1, 8, 4250, 'notes', 'ada/notes', 'main', true, 'https://github.com/ada/notes', now(), '2026-03-01T00:00:00Z'),
      ($1, 99, 4299, 'gone', 'gone/gone', 'main', true, 'https://github.com/gone/gone', now(), '2026-03-01T00:00:00Z')`,
    [ada, bob],
  );
}

describe("organization GitHub links migration", () => {
  test("links each installation to the organizations its installers own, once per installation", async () => {
    const database = await emptyDatabase();
    try {
      await migrate(database.url, migrationsBefore(migration));
      await withClient(database.url, seedUserInstallations);
      expect(await withClient(database.url, backfillTwice)).toEqual({ once: 6, twice: 6 });
      await migrate(database.url, migrations);
      await withClient(database.url, async (client) => {
        const links = await client.query(
          `select organization_id, installation_id, connected_by_user_id, connected_github_login
           from organization_github_installation order by installation_id, organization_id`,
        );
        const byText = (a: unknown[], b: unknown[]) => a.join().localeCompare(b.join());
        const byOrganization = (rows: typeof links.rows) =>
          rows.map((row) => [row.organization_id, row.installation_id, row.connected_by_user_id, row.connected_github_login]).sort(byText);
        expect(byOrganization(links.rows)).toEqual([
          [adaLab, 7, ada, "acme"],
          [adaLab, 8, ada, "ada"],
          [adaShop, 7, ada, "acme"],
          [adaShop, 8, ada, "ada"],
          [bobOwn, 7, bob, "acme"],
          [hopper, 7, bob, "acme"],
        ].sort(byText));

        const installations = await client.query(`select installation_id, account_login, created_at from github_installation order by installation_id`);
        expect(installations.rows.map((row) => [row.installation_id, row.account_login, row.created_at.toISOString()])).toEqual([
          [7, "acme", "2026-01-01T00:00:00.000Z"],
          [8, "ada", "2026-03-01T00:00:00.000Z"],
        ]);

        const cache = await client.query(`select installation_id, repository_id, default_branch from github_repository_cache order by installation_id, repository_id`);
        expect(cache.rows).toEqual([
          { installation_id: 7, repository_id: "4242", default_branch: "trunk" },
          { installation_id: 8, repository_id: "4250", default_branch: "main" },
        ]);

        await client.query(`delete from github_installation where installation_id = 7`);
        const remaining = await client.query(`select installation_id from organization_github_installation group by installation_id`);
        expect(remaining.rows).toEqual([{ installation_id: 8 }]);
        const trigger = await client.query(
          `select count(*)::int as count from pg_trigger where tgrelid = 'organization_github_installation'::regclass and not tgisinternal`,
        );
        expect(trigger.rows[0].count).toBeGreaterThan(0);
      });
    } finally {
      await database.drop();
    }
  }, 60_000);
});
