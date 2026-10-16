import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { checkinRateLimit } from "#/modules/machines/waiting/tables";
import { Database } from "#/server/database.server";

const thisSecond = sql`date_trunc('second', now())`;

/**
 * Counts one request against `key` in the current one-second window and says whether it is within `limit`. Each call
 * commits on its own, so a refused request still counts and the row lock lasts one statement.
 */
export const takeRateLimitSlot = Effect.fn("Checkin.rateLimit")(function* (key: string, limit: number) {
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
