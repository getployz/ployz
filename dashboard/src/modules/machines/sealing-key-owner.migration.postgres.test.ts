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
const migration = "20261102163240_sealing_key_owner";
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

const first = randomUUID();
const second = randomUUID();
const third = randomUUID();
const key = (letter: string) => `${letter.repeat(43)}=`;

async function seedClusterKeys(client: Client) {
  await client.query(
    `insert into organization (id, name, slug) values ($1, 'First', 'first'), ($2, 'Second', 'second'), ($3, 'Third', 'third')`,
    [first, second, third],
  );
  // K is recorded by two Organizations, First earlier, and by First again for a later pairing; L by Third alone.
  await client.query(
    `insert into cluster_sealing_key (organization_id, cluster_key, public_key, created_at) values
      ($1, $4, $6, '2026-01-01T00:00:00Z'),
      ($2, $5, $6, '2026-02-01T00:00:00Z'),
      ($1, $7, $6, '2026-03-01T00:00:00Z'),
      ($3, $8, $9, '2026-01-15T00:00:00Z')`,
    [first, second, third, "a".repeat(64), "b".repeat(64), key("K"), "c".repeat(64), "d".repeat(64), key("L")],
  );
  // Second kept M after forgetting its Servers, so no pairing records it.
  await client.query(
    `insert into organization_sealing_key (organization_id, public_key, created_at) values ($1, $2, '2026-01-20T00:00:00Z')`,
    [second, key("M")],
  );
}

describe("Sealing Key owner migration", () => {
  test("gives each recorded key to its earliest Organization, reports a shared one, and neither fails nor removes a record", async () => {
    const database = await emptyDatabase();
    try {
      await migrate(database.url, migrationsBefore(migration));
      await withClient(database.url, seedClusterKeys);
      await migrate(database.url, migrations);
      await withClient(database.url, async (client) => {
        const owners = await client.query(`select public_key, organization_id from sealing_key_owner order by public_key`);
        expect(owners.rows).toEqual([
          { public_key: key("K"), organization_id: first },
          { public_key: key("L"), organization_id: third },
          { public_key: key("M"), organization_id: second },
        ]);
        expect((await client.query(`select count(*)::int as count from cluster_sealing_key`)).rows).toEqual([{ count: 4 }]);

        const warnings: { message?: string; hint?: string }[] = [];
        client.on("notice", (notice) => warnings.push({ message: notice.message, hint: notice.hint }));
        const sql = readFileSync(join(migrations, migration, "migration.sql"), "utf8");
        const report = sql.split("--> statement-breakpoint").find((part) => part.includes("RAISE WARNING"));
        if (report === undefined) throw new Error("the migration lost its report");
        await client.query(report);
        expect(warnings).toEqual([{
          message: expect.stringContaining("1 Sealing public key(s) are recorded for more than one Organization"),
          hint: expect.stringContaining("List them with"),
        }]);
      });
    } finally {
      await database.drop();
    }
  }, 120_000);
});
