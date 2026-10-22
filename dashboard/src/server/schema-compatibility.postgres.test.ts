import { execFile as execFileCallback } from "node:child_process";
import { promisify } from "node:util";
import { Effect } from "effect";
import { afterAll, beforeAll, expect, it } from "vitest";
import { assertSchemaCompatible } from "#/server/schema-compatibility";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const execFile = promisify(execFileCallback);

let harness: PostgresTestHarness;
beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

const check = () => Effect.runPromise(assertSchemaCompatible({
  databaseUrl: harness.databaseUrl,
  imageVersion: "v0.3.0-test-image",
}).pipe(Effect.scoped));

it("accepts the image's main and analytics migrations", async () => {
  await expect(check()).resolves.toBeUndefined();
});

for (const table of ["__drizzle_migrations", "__drizzle_migrations_analytics"]) {
  it(`refuses an unknown hash in ${table} without changing history`, async () => {
    await harness.pool.query(`insert into drizzle."${table}" (hash, created_at, name) values ($1, 1, $2)`, [
      "migration-not-shipped-by-this-image", "20000101000000_unknown",
    ]);
    try {
      const before = await harness.pool.query(`select * from drizzle."${table}" order by id`);
      await expect(check()).rejects.toThrow(/v0\.3\.0-test-image.*newer.*image/i);
      const after = await harness.pool.query(`select * from drizzle."${table}" order by id`);
      expect(after.rows).toEqual(before.rows);
    } finally {
      await harness.pool.query(`delete from drizzle."${table}" where hash = $1`, ["migration-not-shipped-by-this-image"]);
    }
  });
}

it("accepts a fresh database without either migrations table", async () => {
  try {
    await harness.pool.query("alter schema drizzle rename to saved_drizzle");
    await expect(check()).resolves.toBeUndefined();
  } finally {
    await harness.pool.query("alter schema saved_drizzle rename to drizzle");
  }
});

it("accepts known migration subsets and legacy history without names", async () => {
  await harness.pool.query("alter table drizzle.__drizzle_migrations rename column name to saved_name");
  const removed = await harness.pool.query<{ id: number; hash: string; created_at: string; saved_name: string }>(
    "delete from drizzle.__drizzle_migrations where id = (select max(id) from drizzle.__drizzle_migrations) returning *",
  );
  try {
    await expect(check()).resolves.toBeUndefined();
  } finally {
    for (const row of removed.rows) {
      await harness.pool.query("insert into drizzle.__drizzle_migrations (id, hash, created_at, saved_name) values ($1, $2, $3, $4)", [
        row.id, row.hash, row.created_at, row.saved_name,
      ]);
    }
    await harness.pool.query("alter table drizzle.__drizzle_migrations rename column saved_name to name");
  }
});

it("fails closed when migration assets are missing", async () => {
  await expect(Effect.runPromise(assertSchemaCompatible({
    databaseUrl: harness.databaseUrl,
    imageVersion: "test-image",
    migrationsFolder: "/does-not-exist/ployz-migrations",
  }).pipe(Effect.scoped))).rejects.toThrow();
});

it("db:migrate refuses unknown analytics history before applying pending main migrations", async () => {
  await harness.pool.query("insert into drizzle.__drizzle_migrations_analytics (hash, created_at, name) values ($1, 1, $2)", [
    "migration-not-shipped-by-this-image", "20000101000000_unknown",
  ]);
  const pending = await harness.pool.query<{ id: number; hash: string; created_at: string; name: string }>(
    "delete from drizzle.__drizzle_migrations where id = (select max(id) from drizzle.__drizzle_migrations) returning *",
  );
  try {
    const before = await harness.pool.query("select * from drizzle.__drizzle_migrations order by id");
    await expect(execFile("npm", ["run", "db:migrate"], {
      env: { ...process.env, DATABASE_URL: harness.databaseUrl },
    })).rejects.toThrow(/Use a newer Ployz Cloud image/);
    const after = await harness.pool.query("select * from drizzle.__drizzle_migrations order by id");
    expect(after.rows).toEqual(before.rows);
  } finally {
    await harness.pool.query("delete from drizzle.__drizzle_migrations_analytics where hash = $1", ["migration-not-shipped-by-this-image"]);
    for (const row of pending.rows) {
      await harness.pool.query("insert into drizzle.__drizzle_migrations (id, hash, created_at, name) values ($1, $2, $3, $4)", [
        row.id, row.hash, row.created_at, row.name,
      ]);
    }
  }
}, 30_000);

for (const command of ["start", "start:worker"]) {
  it(`${command} refuses unknown schema before loading its server entrypoint`, async () => {
    await harness.pool.query("insert into drizzle.__drizzle_migrations (hash, created_at, name) values ($1, 1, $2)", [
      "migration-not-shipped-by-this-image", "20000101000000_unknown",
    ]);
    try {
      await expect(execFile("npm", ["run", command], {
        env: { ...process.env, DATABASE_URL: harness.databaseUrl },
      })).rejects.toThrow(/Use a newer Ployz Cloud image/);
    } finally {
      await harness.pool.query("delete from drizzle.__drizzle_migrations where hash = $1", ["migration-not-shipped-by-this-image"]);
    }
  }, 30_000);
}
