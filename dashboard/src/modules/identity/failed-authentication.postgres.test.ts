import { it } from "@effect/vitest";
import * as startServer from "@tanstack/react-start/server";
import { sql } from "drizzle-orm";
import { Cause, Deferred, Effect, Exit, Fiber, Random } from "effect";
import { expect, vi } from "vitest";
import { MANIFEST_HEADER, MANIFEST_REVISION } from "#/modules/commands/commands.server";
import { ClientAddress, FAILED_AUTHENTICATION_WINDOWS, resolveCaller } from "#/modules/identity/caller.server";
import { handleAgentSignup } from "#/routes/api/agent/-signup.handler";
import { handleRunRequest, serveCli } from "#/routes/api/cli/-handlers";
import { Auth, AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { publicErrorResponse } from "#/server/public-error";
import * as run from "#/server/run.server";
import { type StoreCloud, storeTestCloudWith } from "#/test/store-cloud";

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
      message: expect.stringMatching(/^Too many failed sign-ins from this address\. Try again in (\d+ seconds|1 second)\.$/),
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
    expect(held.body).toMatchObject({
      message: expect.stringMatching(/^Too many failed sign-ins from this address\. Try again in \d+ minutes\.$/),
    });
    expect((yield* answer("ployz_elsewhere", "2001:db8:1:3::1")).status).toBe(401);
  })));

it.live("lets no more guesses from one address be checked than its limit, however many arrive at once", () =>
  inCloud(Effect.gen(function* () {
    const answers = yield* Effect.forEach(
      Array.from({ length: minute.limit * 3 }, (_, call) => call),
      (call) => answer(`ployz_burst${call}`, "203.0.113.9"),
      { concurrency: "unbounded" },
    );
    const statuses = answers.map((reply) => reply.status);
    expect(statuses.filter((status) => status === 401)).toHaveLength(minute.limit);
    expect(statuses.filter((status) => status === 429)).toHaveLength(minute.limit * 2);
  })));

it.live("holds the client the proxy names, and says how long in Retry-After, over HTTP", () =>
  inCloud(Effect.gen(function* () {
    const cloud = yield* Effect.context<StoreCloud | Auth>();
    const runInCloud = run.makeEffectRunner<StoreCloud | Auth, never>({ runPromiseExit: (program, options) => Effect.runPromiseExitWith(cloud)(program, options) });
    const runs = vi.spyOn(run, "runAppEffect").mockImplementation(runInCloud as typeof run.runAppEffect);
    // The socket peer is a local reverse proxy, which names the client in X-Forwarded-For.
    const peer = vi.spyOn(startServer, "getRequestIP").mockReturnValue("127.0.0.1");
    yield* Effect.addFinalizer(() => Effect.sync(() => {
      runs.mockRestore();
      peer.mockRestore();
    }));

    const call = (token: string, client: string) => Effect.promise(async () => {
      const request = new Request("http://localhost:3000/api/cli/run/whoami", {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "x-forwarded-for": client },
        body: "{}",
      });
      const response = await serveCli(request, handleRunRequest(request, "whoami"));
      return { status: response.status, retryAfter: response.headers.get("retry-after"), manifest: response.headers.get(MANIFEST_HEADER) };
    });

    for (let guess = 0; guess < minute.limit; guess += 1) expect((yield* call(`ployz_guess${guess}`, "198.51.100.20")).status).toBe(401);
    const held = yield* call("ployz_one-more", "198.51.100.20");
    expect(held.status).toBe(429);
    expect(Number(held.retryAfter)).toBeGreaterThan(0);
    expect(held.manifest).toBe(MANIFEST_REVISION);
    // The proxy's own address is not the key, so another client behind it is untouched.
    expect((yield* call("ployz_neighbour", "198.51.100.21")).status).toBe(401);
  })));

/** The failed-authentication events still counted against `address`. */
const heldAgainst = Effect.fn(function* (address: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ count: number }>(sql`select count(*)::int as count from rate_limit_event where key = ${`failed-auth:${address}`}`, "objects");
  return row?.count;
});

it.live("lets a good token in, and holds nothing against it, when the stale-event prune fails", () =>
  inCloud(Effect.gen(function* () {
    const good = yield* goodToken;
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`insert into rate_limit_event (key, at) values ('agent-signup:192.0.2.1', now() - interval '2 days')`);
    yield* drizzle.execute(sql.raw(`create function refuse_prune() returns trigger language plpgsql as $$
      begin raise exception 'prune refused'; end $$`));
    yield* drizzle.execute(sql.raw(`create trigger refuse_prune before delete on rate_limit_event
      for each row when (old.at < now() - interval '1 day') execute function refuse_prune()`));

    const reply = yield* answer(good, "203.0.113.30").pipe(
      Effect.provideService(Random.Random, { nextDoubleUnsafe: () => 0, nextIntUnsafe: () => 0 }),
    );
    expect(reply.status).toBe(200);
    expect(yield* heldAgainst("203.0.113.30")).toBe(0);
  })));

it.live("holds nothing against an attempt interrupted while its place in the limit commits", () =>
  inCloud(Effect.gen(function* () {
    const good = yield* goodToken;
    const database = yield* Database;
    // A deferred trigger waits on a lock at COMMIT, so the interrupt below lands while the reservation commits.
    yield* database.drizzle.execute(sql.raw(`create function hold_commit() returns trigger language plpgsql as $$
      begin perform pg_advisory_xact_lock(4242); return null; end $$`));
    yield* database.drizzle.execute(sql.raw(`create constraint trigger hold_commit after insert on rate_limit_event
      deferrable initially deferred for each row execute function hold_commit()`));

    const held = yield* Deferred.make<void>();
    const release = yield* Deferred.make<void>();
    const holder = yield* Effect.forkChild(database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`select pg_advisory_xact_lock(4242)`);
      yield* Deferred.succeed(held, undefined);
      yield* Deferred.await(release);
    })));
    yield* Deferred.await(held);

    const attempt = yield* Effect.forkChild(
      resolveCaller(new Headers({ authorization: `Bearer ${good}` })).pipe(Effect.provideService(ClientAddress, "203.0.113.31")),
    );
    const waiting = sql`select 1 from pg_locks where locktype = 'advisory' and objid = 4242 and not granted`;
    yield* Effect.promise(() => vi.waitFor(async () => expect(await Effect.runPromise(database.drizzle.execute(waiting, "objects"))).not.toHaveLength(0)));

    const interrupted = yield* Effect.forkChild(Fiber.interrupt(attempt));
    yield* Deferred.succeed(release, undefined);
    yield* Fiber.join(holder);
    yield* Fiber.join(interrupted);
    expect(yield* heldAgainst("203.0.113.31")).toBe(0);
  })));
