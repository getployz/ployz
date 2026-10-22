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
    const hashes = yield* Effect.try({
      try: () => new Set(readMigrationFiles({ migrationsFolder: resolve(migrationsFolder, history.directory) }).map(migration => migration.hash)),
      catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
    });
    const applied = yield* Effect.tryPromise({
      try: async () => {
        const exists = await client.query<{ present: boolean }>("select to_regclass($1) is not null as present", [`drizzle.${history.table}`]);
        if (!exists.rows[0]?.present) return [];
        const result = await client.query<{ hash: string }>(`select hash from drizzle."${history.table}"`);
        return result.rows;
      },
      catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
    });
    if (applied.some(migration => !hashes.has(migration.hash))) {
      return yield* new UnsupportedDatabaseSchema({
        message: `Ployz Cloud image ${imageVersion} does not ship all migrations recorded in drizzle.${history.table}. Use a newer Ployz Cloud image.`,
      });
    }
  }
});
