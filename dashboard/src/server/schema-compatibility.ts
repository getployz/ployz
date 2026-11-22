import { resolve } from "node:path";
import { readMigrationFiles } from "drizzle-orm/migrator";
import { Data, Effect } from "effect";
import { Client } from "pg";

class SchemaCompatibilityCheckFailure extends Data.TaggedError("SchemaCompatibilityCheckFailure")<{
  readonly cause: unknown;
}> {}

export class UnsupportedDatabaseSchema extends Data.TaggedError("UnsupportedDatabaseSchema")<{
  readonly message: string;
}> {}

/** A row of Drizzle's history. Older rows have no name. */
type AppliedMigration = { readonly hash: string; readonly name?: string | null; readonly created_at?: string | number | null };

/**
 * Migrations main shipped, then renamed and edited, so no image ships their name or hash. Add one here when a
 * migration that reached main is renamed; one folded into the baseline is older than every shipped migration already.
 */
const RETIRED_MIGRATIONS = new Set(["20261011184200_service_act"]);

export const assertSchemaCompatible = Effect.fn("Database.assertSchemaCompatible")(function* ({
  databaseUrl,
  imageVersion,
  migrationsFolder = resolve("drizzle"),
}: {
  readonly databaseUrl: string;
  readonly imageVersion: string;
  readonly migrationsFolder?: string;
}) {
  const client = yield* Effect.acquireRelease(
    Effect.sync(() => new Client({ connectionString: databaseUrl, connectionTimeoutMillis: 10_000, query_timeout: 10_000 })),
    client => Effect.promise(() => client.end()),
  );
  yield* Effect.tryPromise({
    try: () => client.connect(),
    catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
  });
  const shipped = yield* Effect.try({
    try: () => {
      const migrations = readMigrationFiles({ migrationsFolder: resolve(migrationsFolder) });
      if (migrations.length === 0) throw new Error(`No migrations in ${resolve(migrationsFolder)}`);
      return migrations;
    },
    catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
  });
  const names = new Set(shipped.map(migration => migration.name));
  const hashes = new Set(shipped.map(migration => migration.hash));
  const millis = new Set(shipped.map(migration => Number(migration.folderMillis)));
  const oldest = Math.min(...shipped.map(migration => migration.folderMillis));
  const applied = yield* Effect.tryPromise({
    try: async () => {
      const exists = await client.query<{ present: boolean }>("select to_regclass($1) is not null as present", ["drizzle.__drizzle_migrations"]);
      if (!exists.rows[0]?.present) return [];
      const result = await client.query<{ row: AppliedMigration }>("select to_jsonb(t) as row from drizzle.__drizzle_migrations t");
      return result.rows.map(({ row }) => row);
    },
    catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
  });
  const explained = (migration: AppliedMigration) =>
    (migration.name ? names.has(migration.name) || RETIRED_MIGRATIONS.has(migration.name) : millis.has(Number(migration.created_at)))
    || hashes.has(migration.hash)
    || Number(migration.created_at) < oldest;
  if (!applied.every(explained)) {
    return yield* new UnsupportedDatabaseSchema({
      message: `Ployz Cloud image ${imageVersion} does not ship all migrations recorded in drizzle.__drizzle_migrations. Use a newer Ployz Cloud image.`,
    });
  }
});
