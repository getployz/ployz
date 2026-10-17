import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect } from "vitest";
import { verification } from "#/modules/identity/tables";
import { sweepExpiredVerifications } from "#/modules/identity/verification-sweep.server";
import { Database } from "#/server/database.server";
import { storeTestCloud } from "#/test/store-cloud";

const MINUTE_MS = 60 * 1000;

const state = (identifier: string, callbackURL: string, expiresAt: Date) => ({
  identifier,
  value: JSON.stringify({ callbackURL, codeVerifier: "verifier" }),
  expiresAt,
});

/** An abandoned claim sign-in leaves the raw claim secret in its OAuth state until something deletes the row. */
it.live("the sweep deletes OAuth state that has expired and keeps state a sign-in can still use", () =>
  Effect.gen(function* () {
    const cloud = yield* storeTestCloud();
    yield* Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const now = Date.now();
      yield* drizzle.insert(verification).values([
        state("auth-state:abandoned-claim", "/claim/abandoned-secret", new Date(now - MINUTE_MS)),
        state("auth-state:abandoned-sign-in", "/cloud", new Date(now - MINUTE_MS)),
        state("auth-state:live-claim", "/claim/live-secret", new Date(now + 10 * MINUTE_MS)),
      ]);
      yield* sweepExpiredVerifications();
      const left = yield* drizzle.select({ identifier: verification.identifier }).from(verification);
      expect(left.map((row) => row.identifier)).toEqual(["auth-state:live-claim"]);
    }).pipe(Effect.provide(cloud));
  }));
