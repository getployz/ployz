import "@tanstack/react-start/server-only";

import { and, asc, eq, gte, isNotNull, isNull, lt, ne, sql } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { analyticsPlan } from "#/modules/billing/billing.server";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";
import { Conflict, Forbidden, NotFound, Validation } from "#/server/public-error";
import * as analytics from "../adapter";
import { hourBuckets, localDay, localDays } from "../local-time";
import { setSiteAnalytics } from "../salts.server";
import { analyticsGoal, analyticsRequest, analyticsSite } from "../tables";
import {
  analyticsGate,
  type CreateGoalInput,
  DEFAULT_RANGE,
  DEFAULT_WINDOW,
  type DeleteGoalInput,
  isPathPattern,
  isRangeSpec,
  isRequestWindow,
  parsePageFilter,
  partialDays,
  type ReadAnalyticsViewInput,
  type ReadRequestViewInput,
  resolveRange,
  resolveWindow,
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
  const plan = yield* analyticsPlan();
  const open = analyticsGate(plan, plan === "self-host-off" ? undefined : yield* siteRow(organization.id, input.environmentId));
  if (open.gate !== "on") return { gate: open.gate } as const;
  const { site } = open;

  const { drizzle } = yield* Database;
  const goals = yield* drizzle.select({ id: analyticsGoal.id, name: analyticsGoal.name, pathPattern: analyticsGoal.pathPattern })
    .from(analyticsGoal).where(eq(analyticsGoal.environmentId, input.environmentId)).orderBy(asc(analyticsGoal.createdAt));
  const pageviews = and(eq(analyticsRequest.environmentId, input.environmentId), eq(analyticsRequest.kind, "pageview"));
  const [seen] = yield* drizzle.select({ ts: analyticsRequest.ts }).from(analyticsRequest)
    .where(and(pageviews, isNotNull(analyticsRequest.visitorId))).limit(1);
  if (seen === undefined) {
    // Every pageview can come on a day with no salt: then the days they came are all the site has.
    const day = sql<string>`((${analyticsRequest.ts} at time zone ${site.timezone}::text)::date)::text`;
    const lost = yield* drizzle.selectDistinct({ day }).from(analyticsRequest)
      .where(and(pageviews, isNull(analyticsRequest.visitorId))).orderBy(sql`1`);
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
    site: { environmentId: input.environmentId, timezone: site.timezone },
    range,
    filters: page === null ? [] : [page],
    limit: TABLE_ROWS,
  };
  const figures = yield* analytics.figures(q, interval, ["page", "source", "utm_campaign", "language", "host"]);
  const today = localDay(site.timezone, now);
  const byGoal = new Map(figures.goals.map((goal) => [goal.goalId, goal]));
  return {
    gate: "on",
    empty: false,
    timezone: site.timezone,
    range: { start: figures.range.start.getTime(), end: figures.range.end.getTime() },
    clamped: figures.clamped,
    interval,
    overview: figures.overview,
    buckets: figures.buckets.map(({ bucket, pageviews, visitors }) => ({ at: bucket.getTime(), pageviews, visitors })),
    partialDays: partialDays(localDays(site.timezone, figures.range.start, figures.range.end), { today, noSalt: figures.noSaltDays }),
    pages: figures.values("page"),
    sources: figures.values("source"),
    entryPages: figures.entryPages.map(({ value, visitors, visits, bounceRatePercent }) => ({ value, visitors, visits, bounceRatePercent })),
    campaigns: figures.values("utm_campaign"),
    languages: figures.values("language"),
    hosts: figures.values("host"),
    goals: goals.map((goal) => ({ ...goal, visitors: byGoal.get(goal.id)?.visitors ?? 0, conversions: byGoal.get(goal.id)?.conversions ?? 0 })),
  } as const;
});

export type AnalyticsView = Effect.Success<ReturnType<typeof readAnalyticsView>>;

/** The API and Errors views' common ground: the same gate as the Web view, the window as instants, and its services. */
const openRequests = Effect.fn("WebAnalyticsView.openRequests")(function* (actor: Actor, input: ReadRequestViewInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const plan = yield* analyticsPlan();
  const open = analyticsGate(plan, plan === "self-host-off" ? undefined : yield* siteRow(organization.id, input.environmentId));
  if (open.gate !== "on") return { gate: open.gate } as const;
  const { timezone } = open.site;
  const now = yield* Clock.currentTimeMillis;
  const q: analytics.Query = {
    site: { environmentId: input.environmentId, timezone },
    range: resolveWindow(isRequestWindow(input.window) ? input.window : DEFAULT_WINDOW, timezone, now),
    filters: [],
    limit: TABLE_ROWS,
  };
  return { gate: "on", q } as const;
});

const servicesIn = Effect.fn("WebAnalyticsView.services")(function* (environmentId: string, range: analytics.Range) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinct({ service: analyticsRequest.service }).from(analyticsRequest)
    .where(and(
      eq(analyticsRequest.environmentId, environmentId),
      ne(analyticsRequest.kind, "bot"),
      gte(analyticsRequest.ts, range.start),
      lt(analyticsRequest.ts, range.end),
    ))
    .orderBy(asc(analyticsRequest.service));
  return rows.map((row) => row.service);
});

const sample = (row: analytics.RequestSample) => ({ ...row, ts: row.ts.getTime() });

/** Routes by traffic with latency and 5xx, and the slowest requests, over a window. */
export const readApiView = Effect.fn("WebAnalyticsView.readApi")(function* (actor: Actor, input: ReadRequestViewInput) {
  const open = yield* openRequests(actor, input);
  if (open.gate !== "on") return { gate: open.gate } as const;
  const answer = yield* analytics.api(open.q, input.service);
  return {
    gate: "on",
    timezone: open.q.site.timezone,
    range: { start: answer.range.start.getTime(), end: answer.range.end.getTime() },
    clamped: answer.clamped,
    services: yield* servicesIn(input.environmentId, answer.range),
    routes: answer.routes,
    slowest: answer.slowest.map(sample),
  } as const;
});

/** 4xx and 5xx per hour, failures by route and status, and the newest failures, over a window. */
export const readErrorsView = Effect.fn("WebAnalyticsView.readErrors")(function* (actor: Actor, input: ReadRequestViewInput) {
  const open = yield* openRequests(actor, input);
  if (open.gate !== "on") return { gate: open.gate } as const;
  const { timezone } = open.q.site;
  const answer = yield* analytics.errors(open.q, input.service);
  const counted = new Map(answer.hourly.map((bucket) => [bucket.bucket.getTime(), bucket]));
  return {
    gate: "on",
    timezone,
    range: { start: answer.range.start.getTime(), end: answer.range.end.getTime() },
    clamped: answer.clamped,
    services: yield* servicesIn(input.environmentId, answer.range),
    hourly: hourBuckets(timezone, answer.range.start, answer.range.end).map((bucket) => ({
      at: bucket.getTime(),
      clientErrors: counted.get(bucket.getTime())?.clientErrors ?? 0,
      serverErrors: counted.get(bucket.getTime())?.serverErrors ?? 0,
    })),
    byRoute: answer.byRoute,
    newest: answer.newest.map(sample),
  } as const;
});

/** Opens the gate's last step: the plan allows analytics, so the Environment starts counting in the browser's zone. */
export const turnOnAnalytics = Effect.fn("WebAnalyticsView.turnOn")(function* (
  actor: Actor,
  input: TurnOnAnalyticsInput,
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "manage");
  const plan = yield* analyticsPlan();
  if (plan === "self-host-off") return yield* new Forbidden({ message: "Analytics is off on this Cloud." });
  const site = yield* siteRow(organization.id, input.environmentId);
  // A zone chosen once stays: changing it re-rolls every day the site has counted.
  yield* setSiteAnalytics(organization.id, input.environmentId, { enabled: true, ...(site === undefined && { timezone: input.timezone }) });
});

const requireSite = Effect.fn("WebAnalyticsView.requireSite")(function* (actor: Actor, organizationSlug: string, environmentId: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "operate");
  const plan = yield* analyticsPlan();
  if (plan === "self-host-off") return yield* new Forbidden({ message: "Analytics is off on this Cloud." });
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
