import "@tanstack/react-start/server-only";
import { randomBytes } from "node:crypto";
import { TZDate } from "@date-fns/tz";
import { and, asc, eq, gte, inArray } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { owners } from "#/modules/config-store/store-lookup.server";
import { analyticsSalt, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { Validation } from "#/server/public-error";

/** One salt as the ack carries it: its site-local day and that day's `[start, end)` in unix seconds. */
export type AckSalt = { readonly day: string; readonly start: number; readonly end: number; readonly salt: string };
export type SaltAck = { readonly sites: ReadonlyArray<{ readonly namespace: string; readonly salts: ReadonlyArray<AckSalt> }> };

function isTimeZone(timezone: string) {
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: timezone });
    return true;
  } catch {
    return false;
  }
}

/** The site-local calendar day `offset` days from the one containing `now`, as "YYYY-MM-DD". */
export function siteDay(timezone: string, now: number, offset = 0) {
  const local = new TZDate(now, timezone);
  return new Date(Date.UTC(local.getFullYear(), local.getMonth(), local.getDate() + offset)).toISOString().slice(0, 10);
}

/** The unix seconds of the two site-local midnights bounding `day`: 86 400 apart, or 82 800 / 90 000 across DST. */
export function saltWindow(day: string, timezone: string) {
  const utc = new Date(`${day}T00:00:00Z`);
  const midnight = (offset: number) =>
    Math.floor(new TZDate(utc.getUTCFullYear(), utc.getUTCMonth(), utc.getUTCDate() + offset, timezone).getTime() / 1000);
  return { start: midnight(0), end: midnight(1) };
}

const salts = (organizationId: string, environmentId: string, days: ReadonlyArray<string>) =>
  days.map((day) => ({ organizationId, environmentId, day, salt: randomBytes(32) }));

/**
 * Lane 7's switch. Turning a site on issues its site-local today, today+1 and today+2; turning it off deletes nothing,
 * the ack just omits it. `timezone` (IANA) defaults to the site's current one, UTC for a new site.
 */
export const setSiteAnalytics = Effect.fn("WebAnalytics.setSite")(function* (
  organizationId: string,
  environmentId: string,
  setting: { readonly enabled: boolean; readonly timezone?: string },
) {
  if (setting.timezone !== undefined && !isTimeZone(setting.timezone)) {
    return yield* new Validation({ message: `Unknown time zone ${setting.timezone}.`, field: "timezone" });
  }
  const now = yield* Clock.currentTimeMillis;
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [site] = yield* drizzle.insert(analyticsSite)
      .values({ organizationId, environmentId, enabled: setting.enabled, ...(setting.timezone && { timezone: setting.timezone }) })
      .onConflictDoUpdate({
        target: [analyticsSite.organizationId, analyticsSite.environmentId],
        set: { enabled: setting.enabled, ...(setting.timezone && { timezone: setting.timezone }), updatedAt: new Date(now) },
      })
      .returning({ timezone: analyticsSite.timezone });
    if (!setting.enabled || site === undefined) return;
    const days = [0, 1, 2].map((offset) => siteDay(site.timezone, now, offset));
    yield* drizzle.insert(analyticsSalt).values(salts(organizationId, environmentId, days)).onConflictDoNothing();
  }));
});

/** Lane 8's hold: deletes the site's salt for its site-local today, so the ack carries no window for today until one is issued again. */
export const withholdToday = Effect.fn("WebAnalytics.withholdToday")(function* (organizationId: string, environmentId: string) {
  const now = yield* Clock.currentTimeMillis;
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.select({ timezone: analyticsSite.timezone }).from(analyticsSite)
    .where(and(eq(analyticsSite.organizationId, organizationId), eq(analyticsSite.environmentId, environmentId)));
  yield* drizzle.delete(analyticsSalt).where(and(
    eq(analyticsSalt.organizationId, organizationId),
    eq(analyticsSalt.environmentId, environmentId),
    eq(analyticsSalt.day, siteDay(site?.timezone ?? "UTC", now)),
  ));
});

/** Each Environment id of the Organization, with the Namespace a Machine keys its site by. */
const namespacesByEnvironment = Effect.fn("WebAnalytics.namespaces")(function* (organizationId: string) {
  const owned = yield* owners(organizationId);
  const byEnvironment = new Map<string, string>();
  for (const project of new Set(owned.map((namespace) => namespace.project))) {
    const { environments } = yield* readStore(organizationId, { query: "environments", project });
    for (const environment of environments) {
      const namespace = owned.find((candidate) => candidate.project === project && candidate.environment === environment.name);
      if (namespace) byEnvironment.set(environment.id, namespace.namespace);
    }
  }
  return byEnvironment;
});

/**
 * The ack for an accepted push: ensures today+1 and today+2 for every on site of the Organization (never today), then
 * returns each on site's salts from its site-local yesterday on. Sites that are off, or whose Environment has no
 * Namespace, are absent.
 */
export const ackSalts = Effect.fn("WebAnalytics.ack")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const sites = yield* drizzle.select({ environmentId: analyticsSite.environmentId, timezone: analyticsSite.timezone })
    .from(analyticsSite)
    .where(and(eq(analyticsSite.organizationId, organizationId), eq(analyticsSite.enabled, true)));
  if (sites.length === 0) return { sites: [] } satisfies SaltAck;

  const now = yield* Clock.currentTimeMillis;
  yield* drizzle.insert(analyticsSalt)
    .values(sites.flatMap((site) => salts(organizationId, site.environmentId, [siteDay(site.timezone, now, 1), siteDay(site.timezone, now, 2)])))
    .onConflictDoNothing();

  const windows = sites.map((site) => ({ ...site, from: siteDay(site.timezone, now, -1) }));
  const rows = yield* drizzle.select({ environmentId: analyticsSalt.environmentId, day: analyticsSalt.day, salt: analyticsSalt.salt })
    .from(analyticsSalt)
    .where(and(
      eq(analyticsSalt.organizationId, organizationId),
      inArray(analyticsSalt.environmentId, sites.map((site) => site.environmentId)),
      gte(analyticsSalt.day, windows.map((site) => site.from).reduce((earliest, from) => (from < earliest ? from : earliest))),
    ))
    .orderBy(asc(analyticsSalt.day));

  const namespaces = yield* namespacesByEnvironment(organizationId);
  return {
    sites: windows.flatMap((site) => {
      const namespace = namespaces.get(site.environmentId);
      if (namespace === undefined) return [];
      return [{
        namespace,
        salts: rows
          .filter((row) => row.environmentId === site.environmentId && row.day >= site.from)
          .map((row) => ({ day: row.day, ...saltWindow(row.day, site.timezone), salt: Buffer.from(row.salt).toString("base64url") })),
      }];
    }),
  } satisfies SaltAck;
});
