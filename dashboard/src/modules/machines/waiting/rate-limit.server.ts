import "@tanstack/react-start/server-only";
import { and, eq, gt, lt, sql } from "drizzle-orm";
import { Effect, Random } from "effect";
import { checkinRateLimit, rateLimitEvent } from "#/modules/machines/waiting/tables";
import { Database } from "#/server/database.server";

const thisSecond = sql`date_trunc('second', now())`;
const PRUNE_ONE_CALL_IN = 100;

export const countWithinRateLimit = Effect.fn("Checkin.rateLimit")(function* (key: string, limit: number) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle
    .insert(checkinRateLimit)
    .values({ key, windowStart: thisSecond, count: 1 })
    .onConflictDoUpdate({
      target: checkinRateLimit.key,
      set: {
        windowStart: sql`greatest(${checkinRateLimit.windowStart}, ${thisSecond})`,
        count: sql`case when ${checkinRateLimit.windowStart} >= ${thisSecond} then ${checkinRateLimit.count} + 1 else 1 end`,
      },
    })
    .returning({ count: checkinRateLimit.count });
  return row !== undefined && row.count <= limit;
});

export const pruneStaleRateLimits = Effect.fn("Checkin.pruneRateLimits")(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(checkinRateLimit).where(lt(checkinRateLimit.windowStart, thisSecond));
});

export const sometimesPruneStaleRateLimits = Effect.gen(function* () {
  if ((yield* Random.next) * PRUNE_ONE_CALL_IN < 1) yield* pruneStaleRateLimits();
});

export interface RateWindow {
  readonly seconds: number;
  readonly limit: number;
}

export type Admission =
  | { readonly admitted: true }
  | { readonly admitted: false; readonly retryAfterSeconds: number };

const EVENT_RETENTION_SECONDS = 24 * 60 * 60;

export const admitWithinWindows = Effect.fn("RateLimit.admitWithinWindows")(function* (
  key: string,
  windows: ReadonlyArray<RateWindow>,
) {
  if (windows.some((window) => window.seconds > EVENT_RETENTION_SECONDS)) {
    return yield* Effect.die(`a rate window may not outlast the ${EVENT_RETENTION_SECONDS}s event retention`);
  }
  const database = yield* Database;
  const admission: Admission = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`rate-limit:${key}`}))`);
    let retryAfterSeconds = 0;
    for (const window of windows) {
      const [row] = yield* drizzle
        .select({
          count: sql<number>`count(*)::int`,
          retryAfter: sql<number | null>`ceil(extract(epoch from min(${rateLimitEvent.at}) + make_interval(secs => ${window.seconds}) - now()))::int`,
        })
        .from(rateLimitEvent)
        .where(and(
          eq(rateLimitEvent.key, key),
          gt(rateLimitEvent.at, sql`now() - make_interval(secs => ${window.seconds})`),
        ));
      if (row !== undefined && row.count >= window.limit) {
        retryAfterSeconds = Math.max(retryAfterSeconds, row.retryAfter ?? 1, 1);
      }
    }
    if (retryAfterSeconds > 0) return { admitted: false as const, retryAfterSeconds };
    yield* drizzle.insert(rateLimitEvent).values({ key });
    return { admitted: true as const };
  }));
  if ((yield* Random.next) * PRUNE_ONE_CALL_IN < 1) {
    yield* database.drizzle.delete(rateLimitEvent).where(lt(rateLimitEvent.at, sql`now() - make_interval(secs => ${EVENT_RETENTION_SECONDS})`));
  }
  return admission;
});
