import "@tanstack/react-start/server-only";
import { randomBytes } from "node:crypto";
import { and, asc, eq, gte, inArray, sql } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { dayEnd, dayStart, localDay } from "#/modules/web-analytics/local-time";
import { readStore } from "#/modules/config-store/config-store.server";
import { owners } from "#/modules/config-store/store-lookup.server";
import {
  analyticsClickhouseSite,
  analyticsDaily,
  analyticsRollupState,
  analyticsSalt,
  analyticsSessionHourly,
  analyticsSite,
} from "#/modules/web-analytics/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";

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

export function siteDay(timezone: string, now: number, offset = 0) {
  return localDay(timezone, dayStart(timezone, localDay(timezone, now), offset));
}

export function saltWindow(day: string, timezone: string) {
  return { start: dayStart(timezone, day).getTime() / 1000, end: dayEnd(timezone, day).getTime() / 1000 };
}

const salts = (organizationId: string, environmentId: string, days: ReadonlyArray<string>) =>
  days.map((day) => ({ organizationId, environmentId, day, salt: randomBytes(32) }));

/** Locks the site's rollup_state row before its site row, the order ingest and the rollup take them in, and refuses an environment that is gone. */
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
    if (setting.enabled) yield* drizzle.insert(analyticsRollupState).values({ environmentId }).onConflictDoNothing();
    yield* drizzle.select({ environmentId: analyticsRollupState.environmentId }).from(analyticsRollupState)
      .where(eq(analyticsRollupState.environmentId, environmentId))
      .for("update");
    // Read after the lock, so an enable that waited on the environment's delete sees it gone instead of leaving rows.
    const [environment] = yield* drizzle.execute<{ id: string }>(sql`
      select id from config_environment where id = ${environmentId} and organization_id = ${organizationId}`, "objects");
    if (environment === undefined) return yield* new NotFound({ message: "Environment not found." });
    const [previous] = yield* drizzle.select({ timezone: analyticsSite.timezone }).from(analyticsSite)
      .where(and(eq(analyticsSite.organizationId, organizationId), eq(analyticsSite.environmentId, environmentId)))
      .for("update");
    const [site] = yield* drizzle.insert(analyticsSite)
      .values({ organizationId, environmentId, enabled: setting.enabled, ...(setting.timezone && { timezone: setting.timezone }) })
      .onConflictDoUpdate({
        target: [analyticsSite.organizationId, analyticsSite.environmentId],
        set: { enabled: setting.enabled, ...(setting.timezone && { timezone: setting.timezone }), updatedAt: new Date(now) },
      })
      .returning({ timezone: analyticsSite.timezone });
    if (previous !== undefined && site !== undefined && previous.timezone !== site.timezone) {
      yield* drizzle.delete(analyticsDaily).where(eq(analyticsDaily.environmentId, environmentId));
      yield* drizzle.delete(analyticsSessionHourly).where(eq(analyticsSessionHourly.environmentId, environmentId));
      yield* drizzle.update(analyticsRollupState)
        .set({ rolledUpTo: null, dirty: sql`'{}'::date[]`, updatedAt: new Date(now) })
        .where(eq(analyticsRollupState.environmentId, environmentId));
      yield* drizzle.update(analyticsClickhouseSite)
        .set({ rolledUpTo: null, dirty: sql`'{}'::date[]`, reads: false, days: sql`'{}'::jsonb`, updatedAt: new Date(now) })
        .where(eq(analyticsClickhouseSite.environmentId, environmentId));
    }
    if (!setting.enabled || site === undefined) return;
    const days = [0, 1, 2].map((offset) => siteDay(site.timezone, now, offset));
    yield* drizzle.insert(analyticsSalt).values(salts(organizationId, environmentId, days)).onConflictDoNothing();
  }));
});

export const withholdToday = Effect.fn("WebAnalytics.withholdToday")(function* (organizationId: string, environmentId: string) {
  const now = yield* Clock.currentTimeMillis;
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.select({ timezone: analyticsSite.timezone }).from(analyticsSite)
    .where(and(eq(analyticsSite.organizationId, organizationId), eq(analyticsSite.environmentId, environmentId)));
  if (site === undefined) return;
  yield* drizzle.delete(analyticsSalt).where(and(
    eq(analyticsSalt.organizationId, organizationId),
    eq(analyticsSalt.environmentId, environmentId),
    eq(analyticsSalt.day, siteDay(site.timezone, now)),
  ));
});

export const namespacesByEnvironment = Effect.fn("WebAnalytics.namespaces")(function* (organizationId: string) {
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

export const ackSalts = Effect.fn("WebAnalytics.ack")(function* (organizationId: string) {
  if (!(yield* AppConfig).analyticsEnabled) return { sites: [] } satisfies SaltAck;
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
