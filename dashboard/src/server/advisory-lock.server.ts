import "@tanstack/react-start/server-only";
import { Deferred, Effect, Fiber, Option, type Semaphore } from "effect";
import type { SqlError } from "effect/unstable/sql/SqlError";
import { Database, type DatabasePostCommitFailure } from "#/server/database.server";

/**
 * Runs `body` while a transaction of its own holds the advisory lock `lock` takes, so `body`'s queries stay outside that
 * transaction; none when `lock` answers that it is taken. The lock is transaction-scoped, so a dead process holds none.
 * `body` can't be interrupted: work it stopped awaiting could still land after the lock goes. Each holder keeps a pooled
 * connection while its `body` takes more, so `holders` bounds how many hold at once.
 */
export const whileLocked = <A, E, R, LE, LR>(holders: Semaphore.Semaphore, lock: Effect.Effect<boolean, LE, LR>, body: Effect.Effect<A, E, R>) =>
  holders.withPermits(1)(Effect.gen(function* () {
    const database = yield* Database;
    const locked = yield* Deferred.make<boolean, LE | SqlError | DatabasePostCommitFailure>();
    const done = yield* Deferred.make<void>();
    const holder = yield* database.transaction(Effect.gen(function* () {
      const held = yield* lock;
      yield* Deferred.succeed(locked, held);
      if (held) yield* Deferred.await(done);
    })).pipe(Effect.onError((cause) => Deferred.failCause(locked, cause)), Effect.forkChild);
    if (!(yield* Deferred.await(locked))) {
      yield* Fiber.await(holder);
      return Option.none<A>();
    }
    return Option.some(yield* Effect.uninterruptible(body).pipe(Effect.ensuring(Deferred.succeed(done, undefined).pipe(Effect.andThen(Fiber.await(holder))))));
  }));
