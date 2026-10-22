import { resolve } from "node:path";
import { readMigrationFiles } from "drizzle-orm/migrator";
import { Data, Effect } from "effect";
import { Client } from "pg";

export class SchemaCompatibilityCheckFailure extends Data.TaggedError("SchemaCompatibilityCheckFailure")<{
  readonly cause: unknown;
}> {}

export class UnsupportedDatabaseSchema extends Data.TaggedError("UnsupportedDatabaseSchema")<{
  readonly message: string;
}> {}

/** A row of Drizzle's migration history. Older histories have no name; an edited file changes the hash, not the migration. */
type AppliedMigration = { readonly name?: string | null; readonly created_at?: string | number | null };

const histories = [
  { table: "__drizzle_migrations", directory: "." },
  { table: "__drizzle_migrations_analytics", directory: "analytics" },
];

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
  for (const history of histories) {
    const shipped = yield* Effect.try({
      try: () => readMigrationFiles({ migrationsFolder: resolve(migrationsFolder, history.directory) }),
      catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
    });
    const names = new Set(shipped.map(migration => migration.name));
    const millis = new Set(shipped.map(migration => String(migration.folderMillis)));
    const applied = yield* Effect.tryPromise({
      try: async () => {
        const exists = await client.query<{ present: boolean }>("select to_regclass($1) is not null as present", [`drizzle.${history.table}`]);
        if (!exists.rows[0]?.present) return [];
        const result = await client.query<{ row: AppliedMigration }>(`select to_jsonb(t) as row from drizzle."${history.table}" t`);
        return result.rows.map(({ row }) => row);
      },
      catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
    });
    if (applied.some(migration => migration.name ? !names.has(migration.name) : !millis.has(String(migration.created_at)))) {
      return yield* new UnsupportedDatabaseSchema({
        message: `Ployz Cloud image ${imageVersion} does not ship all migrations recorded in drizzle.${history.table}. Use a newer Ployz Cloud image.`,
      });
    }
  }
});
