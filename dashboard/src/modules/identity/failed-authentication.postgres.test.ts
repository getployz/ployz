import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Cause, Effect, Exit } from "effect";
import { expect } from "vitest";
import { ClientAddress, FAILED_AUTHENTICATION_WINDOWS, resolveCaller } from "#/modules/identity/caller.server";
import { handleAgentSignup } from "#/routes/api/agent/-signup.handler";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { publicErrorResponse } from "#/server/public-error";
import { storeTestCloudWith } from "#/test/store-cloud";

const inCloud = <A, E, R>(body: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  return yield* body.pipe(Effect.provide(yield* storeTestCloudWith(AuthLive)));
});

/** A working Organization Token, from an agent signup. */
const goodToken = Effect.gen(function* () {
  const response = yield* handleAgentSignup("198.51.100.1");
  const body = yield* Effect.promise(() => response.json() as Promise<{ token: string }>);
  return body.token;
});

/** What a `ployz` API route answers `token` from `address`: 200 once it is a Caller, else the public error boundary's reply. */
const answer = Effect.fn(function* (token: string | null, address: string) {
  const headers = new Headers(token === null ? {} : { authorization: `Bearer ${token}` });
  const exit = yield* Effect.exit(resolveCaller(headers).pipe(Effect.provideService(ClientAddress, address)));
  if (Exit.isSuccess(exit)) return { status: 200, retryAfter: null, body: null };
  const response = publicErrorResponse(Cause.squash(exit.cause));
  return { status: response.status, retryAfter: response.headers.get("retry-after"), body: yield* Effect.promise(() => response.json()) };
});

const [minute, hour] = FAILED_AUTHENTICATION_WINDOWS;
if (minute === undefined || hour === undefined) throw new Error("expected a minute and an hour window");

/** Ages every recorded failure, as if `seconds` had passed. */
const age = Effect.fn(function* (seconds: number) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`update rate_limit_event set at = at - make_interval(secs => ${seconds}) where key like 'failed-auth:%'`);
});

it.live("answers 429 with Retry-After once an address fails authentication too often, until the window passes", () =>
  inCloud(Effect.gen(function* () {
    const good = yield* goodToken;
    const guesser = "203.0.113.7";

    // Successes cost nothing, however many.
    for (let call = 0; call < minute.limit + 5; call += 1) expect((yield* answer(good, guesser)).status).toBe(200);

    // A bad Organization Token, a bad session and no credential all count.
    for (let call = 0; call < minute.limit; call += 1) {
      const bearer = [`ployz_guess${call}`, `session-guess-${call}`, null][call % 3] ?? null;
      expect((yield* answer(bearer, guesser)).status).toBe(401);
    }
    const held = yield* answer("ployz_one-more", guesser);
    expect(held.status).toBe(429);
    expect(Number(held.retryAfter)).toBeGreaterThan(0);
    expect(Number(held.retryAfter)).toBeLessThanOrEqual(minute.seconds);
    expect(held.body).toEqual({
      _tag: "PublicError",
      code: "RATE_LIMITED",
      message: expect.stringMatching(/^Too many failed sign-ins from this address\. Try again in \d+ seconds\.$/),
    });
    // The address waits as a whole, so a guess can't land while it does.
    expect((yield* answer(good, guesser)).status).toBe(429);

    // Another address is untouched.
    expect((yield* answer(good, "203.0.113.8")).status).toBe(200);
    expect((yield* answer("ployz_other", "203.0.113.8")).status).toBe(401);

    // Once the minute passes, the address is let in again.
    yield* age(minute.seconds + 1);
    expect((yield* answer(good, guesser)).status).toBe(200);
    expect((yield* answer("ployz_again", guesser)).status).toBe(401);
  })));

it.live("holds an address that keeps failing just under the minute limit for the rest of the hour", () =>
  inCloud(Effect.gen(function* () {
    const guesser = "2001:db8:1:2::1";
    let failed = 0;
    while (failed < hour.limit) {
      for (let call = 0; call < minute.limit && failed < hour.limit; call += 1, failed += 1) {
        // Hosts rotate inside their /64; it is still one budget.
        expect((yield* answer(`ployz_guess${failed}`, `2001:db8:1:2::${failed + 1}`)).status).toBe(401);
      }
      yield* age(minute.seconds + 1);
    }
    const held = yield* answer("ployz_one-more", guesser);
    expect(held.status).toBe(429);
    expect(Number(held.retryAfter)).toBeGreaterThan(minute.seconds);
    expect(Number(held.retryAfter)).toBeLessThanOrEqual(hour.seconds);
    expect((yield* answer("ployz_elsewhere", "2001:db8:1:3::1")).status).toBe(401);
  })));
