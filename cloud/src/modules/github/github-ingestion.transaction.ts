import { Effect } from "effect";
import type { Database } from "#/server/database.server";
import { withMutationReceipt } from "#/server/mutation-receipt.server";

export const withGithubTransaction = Effect.fn("Github.withTransaction")(
  function* <A, E, R>(
    work: Effect.Effect<A, E, R | Database>,
    isolationLevel: "read committed" | "repeatable read" = "repeatable read",
  ) {
    return (yield* withMutationReceipt(work, { isolationLevel })).data;
  },
);
