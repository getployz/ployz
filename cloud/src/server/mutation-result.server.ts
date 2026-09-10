import "@tanstack/react-start/server-only";
import type { PgTransactionConfig } from "drizzle-orm/pg-core";
import { Effect } from "effect";
import { Database, sqlErrorFrom } from "#/server/database.server";

function isSerializationFailure(cause: unknown) {
  const sqlError = sqlErrorFrom(cause);
  return sqlError !== undefined && sqlError.reason._tag === "SerializationError";
}

export const withMutationResult = Effect.fn("Database.withMutationResult")(
  function* <A, E, R>(
    program: Effect.Effect<A, E, R | Database>,
    options?: {
      readonly isolationLevel?: PgTransactionConfig["isolationLevel"];
    },
  ) {
    const database = yield* Database;
    const transact = () =>
      database.transaction(
        Effect.map(program, (data) => ({ data })),
        options?.isolationLevel === undefined
          ? undefined
          : { isolationLevel: options.isolationLevel },
      );
    return yield* transact().pipe(
      Effect.retry({ times: 1, while: isSerializationFailure }),
      Effect.annotateLogs({
        isolationLevel: options?.isolationLevel ?? "default",
      }),
    );
  },
);
