import "@tanstack/react-start/server-only";
import { and, eq, gt, lt, sql } from "drizzle-orm";
import { Effect, Random } from "effect";
import ipaddr from "ipaddr.js";
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
  | { readonly admitted: true; readonly event: string }
  | { readonly admitted: false; readonly retryAfterSeconds: number };

const EVENT_RETENTION_SECONDS = 24 * 60 * 60;

/** Counts `key`'s call against every window and admits it when each has room, or says how long until one does. */
export const admitWithinWindows = Effect.fn("RateLimit.admitWithinWindows")(function* (
  key: string,
  windows: ReadonlyArray<RateWindow>,
) {
  if (windows.some((window) => window.seconds > EVENT_RETENTION_SECONDS)) {
    return yield* Effect.die(`a rate window may not outlast the ${EVENT_RETENTION_SECONDS}s event retention`);
  }
  const waits = windows.map(({ seconds, limit }) => {
    const within = sql`${rateLimitEvent.at} > now() - make_interval(secs => ${seconds})`;
    return sql`case when count(*) filter (where ${within}) >= ${limit}
      then greatest(1, ceil(extract(epoch from min(${rateLimitEvent.at}) filter (where ${within}) + make_interval(secs => ${seconds}) - now())))
      else 0 end`;
  });
  const longest = Math.max(...windows.map((window) => window.seconds));
  const database = yield* Database;
  const admission: Admission = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`rate-limit:${key}`}))`);
    const [row] = yield* drizzle
      .select({ retryAfterSeconds: sql<number>`greatest(0, ${sql.join(waits, sql`, `)})::int` })
      .from(rateLimitEvent)
      .where(and(eq(rateLimitEvent.key, key), gt(rateLimitEvent.at, sql`now() - make_interval(secs => ${longest})`)));
    const retryAfterSeconds = row?.retryAfterSeconds ?? 0;
    if (retryAfterSeconds > 0) return { admitted: false as const, retryAfterSeconds };
    const [event] = yield* drizzle.insert(rateLimitEvent).values({ key }).returning({ id: rateLimitEvent.id });
    if (event === undefined) return yield* Effect.die("a rate limit event insert returned no row");
    return { admitted: true as const, event: event.id };
  }));
  if ((yield* Random.next) * PRUNE_ONE_CALL_IN < 1) {
    yield* database.drizzle.delete(rateLimitEvent).where(lt(rateLimitEvent.at, sql`now() - make_interval(secs => ${EVENT_RETENTION_SECONDS})`));
  }
  return admission;
});

/** Uncounts an admitted call, for a limit that holds only the calls that turn out to fail. */
export const forgetRateEvent = Effect.fn("RateLimit.forgetRateEvent")(function* (event: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(rateLimitEvent).where(eq(rateLimitEvent.id, event));
});

/** One host rotates freely inside its IPv6 /64, so that prefix is the unit a limit can hold. */
export const rateLimitAddress = (clientIp: string) => {
  if (!ipaddr.isValid(clientIp)) return clientIp;
  const address = ipaddr.process(clientIp);
  if (address.range() === "unspecified") return "unknown";
  if (address.kind() === "ipv4") return address.toString();
  return `${ipaddr.IPv6.networkAddressFromCIDR(`${address.toString()}/64`).toString()}/64`;
};
