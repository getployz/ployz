import "@tanstack/react-start/server-only";
import { lt } from "drizzle-orm";
import { Data, Effect } from "effect";
import { verification } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";
import { checkedNow } from "#/db/tables";

export class VerificationSweepFailure extends Data.TaggedError("VerificationSweepFailure")<{
  readonly cause: unknown;
}> {}

/**
 * Deletes expired verification rows. better-auth only deletes one when something reads it, so the OAuth state of a
 * sign-in nobody finished, claim secret in its callback URL included, otherwise stays forever.
 */
export const sweepExpiredVerifications = Effect.fn("Verification.sweepExpired")(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(verification).where(lt(verification.expiresAt, checkedNow()))
    .pipe(Effect.mapError((cause) => new VerificationSweepFailure({ cause })));
});
