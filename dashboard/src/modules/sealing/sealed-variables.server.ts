import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { Database } from "#/server/database.server";

/** Whether the Organization's `sealedVariables` flag is on: its `config_sealed_variables` row, which nothing in Cloud inserts. */
export const readSealedVariables = Effect.fn("SealedVariables.read")(function* (organizationId: string) {
  // The Store makes the table when it opens.
  yield* cloudStore;
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ on: number }>(
    sql`select 1 as "on" from config_sealed_variables where organization_id = ${organizationId}`,
    "objects",
  );
  return rows.length > 0;
});
