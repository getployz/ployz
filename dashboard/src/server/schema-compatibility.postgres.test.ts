import { execFile as execFileCallback } from "node:child_process";
import { cp, mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { Effect } from "effect";
import { afterAll, beforeAll, expect, it } from "vitest";
import { assertSchemaCompatible } from "#/server/schema-compatibility";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const execFile = promisify(execFileCallback);

let harness: PostgresTestHarness;
beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

/** A migration timestamp later than any this image ships. */
const NEWER = Date.UTC(2099, 0, 1);

const check = () => Effect.runPromise(assertSchemaCompatible({
  databaseUrl: harness.databaseUrl,
  imageVersion: "v0.3.0-test-image",
}).pipe(Effect.scoped));

it("accepts the image's main and analytics migrations", async () => {
  await expect(check()).resolves.toBeUndefined();
});

for (const table of ["__drizzle_migrations", "__drizzle_migrations_analytics"]) {
  it(`refuses a migration newer than the image in ${table} without changing history`, async () => {
    await harness.pool.query(`insert into drizzle."${table}" (hash, created_at, name) values ($1, $2, $3)`, [
      "migration-not-shipped-by-this-image", NEWER, "20990101000000_unknown",
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

it("accepts a shipped migration whose file was edited after it was applied", async () => {
  const { rows: [first] } = await harness.pool.query<{ id: number; hash: string }>(
    "select id, hash from drizzle.__drizzle_migrations order by id limit 1",
  );
  await harness.pool.query("update drizzle.__drizzle_migrations set hash = 'hash-of-the-file-before-an-edit' where id = $1", [first?.id]);
  try {
    await expect(check()).resolves.toBeUndefined();
  } finally {
    await harness.pool.query("update drizzle.__drizzle_migrations set hash = $2 where id = $1", [first?.id, first?.hash]);
  }
});

/** Inserts history rows for one test and removes them after. */
async function withRows(rows: readonly { id?: number; hash: string; createdAt: number; name: string }[], body: () => Promise<void>) {
  try {
    for (const row of rows) {
      await harness.pool.query("insert into drizzle.__drizzle_migrations (id, hash, created_at, name) values (coalesce($1, nextval(pg_get_serial_sequence('drizzle.__drizzle_migrations', 'id'))), $2, $3, $4)", [
        row.id ?? null, row.hash, row.createdAt, row.name,
      ]);
    }
    await body();
  } finally {
    await harness.pool.query("delete from drizzle.__drizzle_migrations where hash = any($1)", [rows.map((row) => row.hash)]);
  }
}

it("accepts history for a migration main retired, or folded into the image's baseline", async () => {
  await withRows([
    { hash: "retired-service-act", createdAt: Date.UTC(2026, 9, 11, 18, 42), name: "20261011184200_service_act" },
    { hash: "folded-into-baseline", createdAt: Date.UTC(2026, 8, 25), name: "20260925000000_folded" },
  ], () => expect(check()).resolves.toBeUndefined());
});

it("accepts the last applied migration under its old name when the image ships it renamed", async () => {
  const { rows: [last] } = await harness.pool.query<{ id: number; name: string }>("select id, name from drizzle.__drizzle_migrations order by id desc limit 1");
  await harness.pool.query("update drizzle.__drizzle_migrations set name = '20990101000000_old_name' where id = $1", [last?.id]);
  try {
    await expect(check()).resolves.toBeUndefined();
  } finally {
    await harness.pool.query("update drizzle.__drizzle_migrations set name = $2 where id = $1", [last?.id, last?.name]);
  }
});

it("refuses a newer image's migration applied before migrations this image ships", async () => {
  await withRows([
    { id: -1, hash: "newer-image-between-two-shipped", createdAt: Date.UTC(2026, 9, 1), name: "20261001000000_not_shipped" },
  ], () => expect(check()).rejects.toThrow(/Use a newer Ployz Cloud image/));
});

it("refuses a migration merged out of order that the image doesn't ship, applied after its own", async () => {
  // Older than the image's newest migration, as #2960's rollback past deployment_cleanup would leave it.
  await harness.pool.query("insert into drizzle.__drizzle_migrations (hash, created_at, name) values ($1, $2, $3)", [
    "migration-merged-out-of-order", Date.UTC(2026, 9, 1), "20261001000000_not_shipped",
  ]);
  try {
    await expect(check()).rejects.toThrow(/Use a newer Ployz Cloud image/);
  } finally {
    await harness.pool.query("delete from drizzle.__drizzle_migrations where hash = $1", ["migration-merged-out-of-order"]);
  }
});

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

it("refuses an image older than Generated Values once the database has them (#3477)", async () => {
  // v0.3.5 shipped every migration but this one; it can't read a Generated Value, so it must not start silently.
  const older = await mkdtemp(join(tmpdir(), "ployz-migrations-"));
  try {
    await cp(resolve("drizzle"), older, { recursive: true, filter: (source) => !source.endsWith("_generated_variables") });
    await expect(Effect.runPromise(assertSchemaCompatible({
      databaseUrl: harness.databaseUrl,
      imageVersion: "v0.3.5",
      migrationsFolder: older,
    }).pipe(Effect.scoped))).rejects.toThrow(/v0\.3\.5 does not ship all migrations.*Use a newer Ployz Cloud image/);
  } finally {
    await rm(older, { recursive: true });
  }
});

it("fails closed when the image ships no migrations", async () => {
  const empty = await mkdtemp(join(tmpdir(), "ployz-migrations-"));
  await mkdir(join(empty, "analytics"));
  try {
    await expect(Effect.runPromise(assertSchemaCompatible({
      databaseUrl: harness.databaseUrl,
      imageVersion: "test-image",
      migrationsFolder: empty,
    }).pipe(Effect.scoped))).rejects.toThrow();
  } finally {
    await rm(empty, { recursive: true });
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
  await harness.pool.query("insert into drizzle.__drizzle_migrations_analytics (hash, created_at, name) values ($1, $2, $3)", [
    "migration-not-shipped-by-this-image", NEWER, "20990101000000_unknown",
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
    await harness.pool.query("insert into drizzle.__drizzle_migrations (hash, created_at, name) values ($1, $2, $3)", [
      "migration-not-shipped-by-this-image", NEWER, "20990101000000_unknown",
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

it("PLOYZ_ALLOW_OLDER_IMAGE=1 lets an older image start, with a warning, so a rollback is never locked out", async () => {
  await harness.pool.query("insert into drizzle.__drizzle_migrations (hash, created_at, name) values ($1, $2, $3)", [
    "migration-not-shipped-by-this-image", NEWER, "20990101000000_unknown",
  ]);
  try {
    const { stderr } = await execFile("node", ["scripts/check-schema.ts"], {
      env: { ...process.env, DATABASE_URL: harness.databaseUrl, PLOYZ_ALLOW_OLDER_IMAGE: "1" },
    });
    expect(stderr).toMatch(/PLOYZ_ALLOW_OLDER_IMAGE.*Use a newer Ployz Cloud image/s);
  } finally {
    await harness.pool.query("delete from drizzle.__drizzle_migrations where hash = $1", ["migration-not-shipped-by-this-image"]);
  }
}, 30_000);
