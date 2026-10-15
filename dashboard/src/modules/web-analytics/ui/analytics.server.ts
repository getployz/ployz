import "@tanstack/react-start/server-only";

import { and, asc, eq, gt, gte, lte } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { analyticsPlan } from "#/modules/billing/billing.server";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";
import { Conflict, Forbidden, NotFound, Validation } from "#/server/public-error";
import * as analytics from "../adapter";
import { localDay, localDays } from "../local-time";
import { setSiteAnalytics } from "../salts.server";
import { analyticsGapDaily, analyticsGoal, analyticsPageview, analyticsSite } from "../tables";
import {
  analyticsGate,
  type CreateGoalInput,
  DEFAULT_RANGE,
  type DeleteGoalInput,
  isPathPattern,
  isRangeSpec,
  parsePageFilter,
  partialDays,
  type ReadAnalyticsViewInput,
  resolveRange,
  type TurnOnAnalyticsInput,
} from "./analytics-model";

const TABLE_ROWS = 10;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const siteRow = Effect.fn("WebAnalyticsView.site")(function* (organizationId: string, environmentId: string) {
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.select({ enabled: analyticsSite.enabled, timezone: analyticsSite.timezone }).from(analyticsSite)
    .where(and(eq(analyticsSite.organizationId, organizationId), eq(analyticsSite.environmentId, environmentId)));
  return site;
});

/** An Environment's analytics for one range: nothing past the plan gate is read unless the gate is open. */
export const readAnalyticsView = Effect.fn("WebAnalyticsView.read")(function* (actor: Actor, input: ReadAnalyticsViewInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const plan = yield* analyticsPlan(organization.id);
  const open = analyticsGate(plan, plan === "unpaid" || plan === "self-host-off" ? undefined : yield* siteRow(organization.id, input.environmentId));
  if (open.gate !== "on") return { gate: open.gate } as const;
  const { site } = open;

  const { drizzle } = yield* Database;
  const goals = yield* drizzle.select({ id: analyticsGoal.id, name: analyticsGoal.name, pathPattern: analyticsGoal.pathPattern })
    .from(analyticsGoal).where(eq(analyticsGoal.environmentId, input.environmentId)).orderBy(asc(analyticsGoal.createdAt));
  const [seen] = yield* drizzle.select({ ts: analyticsPageview.ts }).from(analyticsPageview)
    .where(eq(analyticsPageview.environmentId, input.environmentId)).limit(1);
  if (seen === undefined) {
    // Every visit can be dropped, for a day with no salt: then the count of lost visits is all the site has.
    const lost = yield* drizzle.select({ day: analyticsGapDaily.day }).from(analyticsGapDaily)
      .where(and(eq(analyticsGapDaily.environmentId, input.environmentId), gt(analyticsGapDaily.count, 0))).orderBy(asc(analyticsGapDaily.day));
    return {
      gate: "on",
      empty: true,
      timezone: site.timezone,
      lostDays: lost.map(({ day }) => day),
      goals: goals.map((goal) => ({ ...goal, visitors: 0, conversions: 0 })),
    } as const;
  }

  const now = yield* Clock.currentTimeMillis;
  const { range, interval } = resolveRange(isRangeSpec(input.range) ? input.range : DEFAULT_RANGE, site.timezone, now);
  const page = parsePageFilter(input.page);
  const q: analytics.Query = {
    site: { environmentId: input.environmentId, timezone: site.timezone, retentionMonths: analytics.BASE_RETENTION_MONTHS },
    range,
    filters: page === null ? [] : [page],
    limit: TABLE_ROWS,
  };
  const [overview, series, pages, sources, entryPages, campaigns, languages, hosts, conversions, gaps] = yield* Effect.all([
    analytics.overview(q),
    analytics.timeseries(q, interval),
    analytics.topPages(q),
    analytics.sources(q),
    analytics.entryPages(q),
    analytics.breakdown(q, "utm_campaign"),
    analytics.breakdown(q, "language"),
    analytics.breakdown(q, "host"),
    analytics.goalConversions(q),
    drizzle.select({ day: analyticsGapDaily.day, count: analyticsGapDaily.count }).from(analyticsGapDaily).where(and(
      eq(analyticsGapDaily.environmentId, input.environmentId),
      gte(analyticsGapDaily.day, localDay(site.timezone, range.start)),
      lte(analyticsGapDaily.day, localDay(site.timezone, range.end.getTime() - 1)),
    )),
  ], { concurrency: "unbounded" });
  const byGoal = new Map(conversions.goals.map((goal) => [goal.goalId, goal]));
  return {
    gate: "on",
    empty: false,
    timezone: site.timezone,
    range: { start: overview.range.start.getTime(), end: overview.range.end.getTime() },
    clamped: overview.clamped,
    interval,
    overview: overview.totals,
    buckets: series.buckets.map(({ bucket, pageviews, visitors }) => ({ at: bucket.getTime(), pageviews, visitors })),
    partialDays: partialDays(localDays(site.timezone, overview.range.start, overview.range.end), gaps, localDay(site.timezone, now)),
    pages: pages.values,
    sources: sources.values,
    entryPages: entryPages.values.map(({ value, visitors, visits, bounceRatePercent }) => ({ value, visitors, visits, bounceRatePercent })),
    campaigns: campaigns.values,
    languages: languages.values,
    hosts: hosts.values,
    goals: goals.map((goal) => ({ ...goal, visitors: byGoal.get(goal.id)?.visitors ?? 0, conversions: byGoal.get(goal.id)?.conversions ?? 0 })),
  } as const;
});

export type AnalyticsView = Effect.Success<ReturnType<typeof readAnalyticsView>>;

/** Opens the gate's last step: the plan allows analytics, so the Environment starts counting in the browser's zone. */
export const turnOnAnalytics = Effect.fn("WebAnalyticsView.turnOn")(function* (
  actor: Actor,
  input: TurnOnAnalyticsInput,
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "manage");
  const plan = yield* analyticsPlan(organization.id);
  if (plan === "unpaid" || plan === "self-host-off") return yield* new Forbidden({ message: "Analytics is not available on this plan." });
  const site = yield* siteRow(organization.id, input.environmentId);
  // A zone chosen once stays: changing it re-rolls every day the site has counted.
  yield* setSiteAnalytics(organization.id, input.environmentId, { enabled: true, ...(site === undefined && { timezone: input.timezone }) });
});

const requireSite = Effect.fn("WebAnalyticsView.requireSite")(function* (actor: Actor, organizationSlug: string, environmentId: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "operate");
  const plan = yield* analyticsPlan(organization.id);
  if (plan === "unpaid" || plan === "self-host-off") return yield* new Forbidden({ message: "Analytics is not available on this plan." });
  const site = yield* siteRow(organization.id, environmentId);
  if (site === undefined || !site.enabled) return yield* new NotFound({ message: "Analytics is not on for this Environment." });
});

export const createGoal = Effect.fn("WebAnalyticsView.createGoal")(function* (
  actor: Actor,
  input: CreateGoalInput,
) {
  if (!isPathPattern(input.pathPattern)) {
    return yield* new Validation({ message: "A goal's path starts with / and has no spaces.", field: "pathPattern", userFacing: true });
  }
  yield* requireSite(actor, input.organizationSlug, input.environmentId);
  const { drizzle } = yield* Database;
  const saved = yield* drizzle.insert(analyticsGoal)
    .values({ environmentId: input.environmentId, name: input.name.trim(), pathPattern: input.pathPattern })
    .onConflictDoNothing({ target: [analyticsGoal.environmentId, analyticsGoal.pathPattern] })
    .returning({ id: analyticsGoal.id });
  if (saved.length === 0) return yield* new Conflict({ message: "A goal already counts that path.", userFacing: true });
});

export const deleteGoal = Effect.fn("WebAnalyticsView.deleteGoal")(function* (
  actor: Actor,
  input: DeleteGoalInput,
) {
  yield* requireSite(actor, input.organizationSlug, input.environmentId);
  if (!UUID.test(input.id)) return;
  const { drizzle } = yield* Database;
  yield* drizzle.delete(analyticsGoal).where(and(eq(analyticsGoal.environmentId, input.environmentId), eq(analyticsGoal.id, input.id)));
});
