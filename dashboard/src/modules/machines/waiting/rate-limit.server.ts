import "@tanstack/react-start/server-only";
import { lt, sql } from "drizzle-orm";
import { Effect, Random } from "effect";
import { checkinRateLimit } from "#/modules/machines/waiting/tables";
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
        windowStart: thisSecond,
        count: sql`case when ${checkinRateLimit.windowStart} = ${thisSecond} then ${checkinRateLimit.count} + 1 else 1 end`,
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
