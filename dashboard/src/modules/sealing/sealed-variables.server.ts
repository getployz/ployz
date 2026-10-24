import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";

/** Whether the Organization's `sealedVariables` flag is on: its `config_sealed_variables` row, which nothing in Cloud inserts. */
export const readSealedVariables = Effect.fn("SealedVariables.read")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  // The Store makes the table when it opens; until then every flag is off. Checking first keeps a caller's transaction alive.
  const [table] = yield* drizzle.execute<{ present: boolean }>(
    sql`select to_regclass('config_sealed_variables') is not null as present`,
    "objects",
  );
  if (table?.present !== true) return false;
  const rows = yield* drizzle.execute<{ on: number }>(
    sql`select 1 as "on" from config_sealed_variables where organization_id = ${organizationId}`,
    "objects",
  );
  return rows.length > 0;
});
