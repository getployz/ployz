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
  | { readonly admitted: true }
  | { readonly admitted: false; readonly retryAfterSeconds: number };

const EVENT_RETENTION_SECONDS = 24 * 60 * 60;

const outlastsRetention = (windows: ReadonlyArray<RateWindow>) =>
  windows.some((window) => window.seconds > EVENT_RETENTION_SECONDS)
    ? Effect.die(`a rate window may not outlast the ${EVENT_RETENTION_SECONDS}s event retention`)
    : Effect.void;

/** Seconds until `key`'s events fit every window again, or 0 when they already do. Records nothing. */
export const secondsUntilWithinWindows = Effect.fn("RateLimit.secondsUntilWithinWindows")(function* (
  key: string,
  windows: ReadonlyArray<RateWindow>,
) {
  yield* outlastsRetention(windows);
  const { drizzle } = yield* Database;
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
  return retryAfterSeconds;
});

const sometimesPruneStaleEvents = Effect.gen(function* () {
  if ((yield* Random.next) * PRUNE_ONE_CALL_IN >= 1) return;
  const { drizzle } = yield* Database;
  yield* drizzle.delete(rateLimitEvent).where(lt(rateLimitEvent.at, sql`now() - make_interval(secs => ${EVENT_RETENTION_SECONDS})`));
});

/** Counts one event against `key`, for a limit that counts only some calls, such as failures. */
export const recordRateEvent = Effect.fn("RateLimit.recordRateEvent")(function* (key: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(rateLimitEvent).values({ key });
  yield* sometimesPruneStaleEvents;
});

export const admitWithinWindows = Effect.fn("RateLimit.admitWithinWindows")(function* (
  key: string,
  windows: ReadonlyArray<RateWindow>,
) {
  yield* outlastsRetention(windows);
  const database = yield* Database;
  const admission: Admission = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`rate-limit:${key}`}))`);
    const retryAfterSeconds = yield* secondsUntilWithinWindows(key, windows);
    if (retryAfterSeconds > 0) return { admitted: false as const, retryAfterSeconds };
    yield* drizzle.insert(rateLimitEvent).values({ key });
    return { admitted: true as const };
  }));
  yield* sometimesPruneStaleEvents;
  return admission;
});

/** One host rotates freely inside its IPv6 /64, so that prefix is the unit a limit can hold. */
export const rateLimitAddress = (clientIp: string) => {
  if (!ipaddr.isValid(clientIp)) return clientIp;
  const address = ipaddr.process(clientIp);
  if (address.range() === "unspecified") return "unknown";
  if (address.kind() === "ipv4") return address.toString();
  return `${ipaddr.IPv6.networkAddressFromCIDR(`${address.toString()}/64`).toString()}/64`;
};
