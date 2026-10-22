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
    const shipped = yield* Effect.try({
      try: () => readMigrationFiles({ migrationsFolder: resolve(migrationsFolder, history.directory) }),
      catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
    });
    const shippedNewest = Math.max(0, ...shipped.map(migration => migration.folderMillis));
    const appliedNewest = yield* Effect.tryPromise({
      try: async () => {
        const exists = await client.query<{ present: boolean }>("select to_regclass($1) is not null as present", [`drizzle.${history.table}`]);
        if (!exists.rows[0]?.present) return 0;
        const result = await client.query<{ newest: string | null }>(`select max(created_at) as newest from drizzle."${history.table}"`);
        return Number(result.rows[0]?.newest ?? 0);
      },
      catch: cause => new SchemaCompatibilityCheckFailure({ cause }),
    });
    if (appliedNewest > shippedNewest) {
      return yield* new UnsupportedDatabaseSchema({
        message: `Ployz Cloud image ${imageVersion} does not ship all migrations recorded in drizzle.${history.table}. Use a newer Ployz Cloud image.`,
      });
    }
  }
});
