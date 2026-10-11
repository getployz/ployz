import "@tanstack/react-start/server-only";
import { and, asc, desc, eq, gte, isNull, lte, or, sql } from "drizzle-orm";
import type { PgColumn } from "drizzle-orm/pg-core";
import { Clock, Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { Database } from "#/server/database.server";
import { clampSince } from "./retention";
import { uptimeCheck, uptimeIncident, uptimeResult, uptimeResultHourly } from "./tables";
import {
  type EnvironmentUptimeCheck,
  IncidentEvidence,
  type ReadEnvironmentUptimeInput,
  type ReadServiceUptimeInput,
  type ReadServiceUptimeSeriesInput,
  type ServiceUptime,
  UPTIME_RANGE_MS,
  type UptimePoint,
  type UptimeRange,
} from "./uptime-status";

const DAY_MS = 86_400_000;
const HISTORY_DAYS = 90;

/**
 * The environment's checks, each with when its open incident opened. Both tables are filtered by the actor's
 * organization, so another organization's environment reads empty.
 */
export const readEnvironmentUptime = Effect.fn("Uptime.readEnvironment")(function* (
  actor: Actor,
  input: ReadEnvironmentUptimeInput,
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ serviceId: uptimeCheck.serviceId, url: uptimeCheck.url, openedAt: uptimeIncident.openedAt })
    .from(uptimeCheck)
    .leftJoin(uptimeIncident, and(
      eq(uptimeIncident.checkId, uptimeCheck.id),
      eq(uptimeIncident.organizationId, organization.id),
      isNull(uptimeIncident.closedAt),
    ))
    .where(and(eq(uptimeCheck.organizationId, organization.id), eq(uptimeCheck.environmentId, input.environmentId)))
    .orderBy(asc(uptimeCheck.serviceId));
  return rows.map((row): EnvironmentUptimeCheck => ({
    serviceId: row.serviceId,
    url: row.url,
    openIncidentAt: row.openedAt?.toISOString() ?? null,
  }));
});

/** The Service's check in the actor's organization, or undefined when it has none. */
const findCheck = Effect.fn("Uptime.findCheck")(function* (organizationId: string, input: ReadServiceUptimeInput) {
  const { drizzle } = yield* Database;
  const [check] = yield* drizzle
    .select({ id: uptimeCheck.id, url: uptimeCheck.url, intervalSeconds: uptimeCheck.intervalSeconds })
    .from(uptimeCheck)
    .where(and(
      eq(uptimeCheck.organizationId, organizationId),
      eq(uptimeCheck.environmentId, input.environmentId),
      eq(uptimeCheck.serviceId, input.serviceId),
    ));
  return check;
});

const ofCheck = (checkId: string, organizationId: string) =>
  (table: { checkId: PgColumn; organizationId: PgColumn }) =>
    and(eq(table.checkId, checkId), eq(table.organizationId, organizationId));

const decodeEvidence = Schema.decodeUnknownEffect(IncidentEvidence);

/** The Service's check with its status, uptime per range, 90 days of history and the incidents in them. Null without a check. */
export const readServiceUptime = Effect.fn("Uptime.readService")(function* (actor: Actor, input: ReadServiceUptimeInput) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const now = new Date(yield* Clock.currentTimeMillis);
  const check = yield* findCheck(organization.id, input);
  if (check === undefined) return null;
  const of = ofCheck(check.id, organization.id);

  const [open] = yield* drizzle
    .select({ id: uptimeIncident.id })
    .from(uptimeIncident)
    .where(and(of(uptimeIncident), isNull(uptimeIncident.closedAt)));
  const [probed] = open !== undefined ? [] : yield* drizzle
    .select({ at: uptimeResult.scheduledAt })
    .from(uptimeResult)
    .where(and(of(uptimeResult), gte(uptimeResult.scheduledAt, clampSince(organization.id, "raw", new Date(0), now))))
    .limit(1);
  const status = open !== undefined ? "unreachable" : probed === undefined ? "pending" : "up";

  const hourlySince = (range: UptimeRange) =>
    clampSince(organization.id, "hourly", new Date(now.getTime() - UPTIME_RANGE_MS[range]), now);
  const inRange = (range: UptimeRange) => sql`${uptimeResultHourly.hour} >= ${hourlySince(range).toISOString()}`;
  const passing = (range: UptimeRange) => sql<number | null>`
    sum(${uptimeResultHourly.probes} - ${uptimeResultHourly.failures}) FILTER (WHERE ${inRange(range)})`.mapWith(Number);
  const counted = (range: UptimeRange) =>
    sql<number | null>`sum(${uptimeResultHourly.probes}) FILTER (WHERE ${inRange(range)})`.mapWith(Number);
  const [sums] = yield* drizzle
    .select({
      pass24h: passing("24h"), probes24h: counted("24h"),
      pass7d: passing("7d"), probes7d: counted("7d"),
      pass90d: passing("90d"), probes90d: counted("90d"),
    })
    .from(uptimeResultHourly)
    .where(and(of(uptimeResultHourly), inRange("90d")));
  const fraction = (pass: number | null | undefined, probes: number | null | undefined) =>
    probes === null || probes === undefined || probes === 0 ? null : (pass ?? 0) / probes;
  const uptime = {
    "24h": fraction(sums?.pass24h, sums?.probes24h),
    "7d": fraction(sums?.pass7d, sums?.probes7d),
    "90d": fraction(sums?.pass90d, sums?.probes90d),
  };

  const today = Math.floor(now.getTime() / DAY_MS) * DAY_MS;
  const firstDay = new Date(today - (HISTORY_DAYS - 1) * DAY_MS);
  const day = sql<Date>`date_trunc('day', ${uptimeResultHourly.hour}, 'UTC')`.mapWith(uptimeResultHourly.hour);
  const checkedDays = yield* drizzle
    .select({ day })
    .from(uptimeResultHourly)
    .where(and(of(uptimeResultHourly),
      gte(uptimeResultHourly.hour, clampSince(organization.id, "hourly", firstDay, now))))
    .groupBy(day);
  const checked = new Set(checkedDays.map((row) => row.day.getTime()));
  const days = Array.from({ length: HISTORY_DAYS }, (_, index) => {
    const start = firstDay.getTime() + index * DAY_MS;
    return { start: new Date(start).toISOString(), checked: checked.has(start) };
  });

  const incidentsSince = clampSince(organization.id, "incidents", new Date(now.getTime() - UPTIME_RANGE_MS["90d"]), now);
  const incidentRows = yield* drizzle
    .select({
      id: uptimeIncident.id,
      openedAt: uptimeIncident.openedAt,
      closedAt: uptimeIncident.closedAt,
      openedBy: uptimeIncident.openedBy,
    })
    .from(uptimeIncident)
    .where(and(
      of(uptimeIncident),
      lte(uptimeIncident.openedAt, now),
      or(isNull(uptimeIncident.closedAt), gte(uptimeIncident.closedAt, incidentsSince)),
    ))
    .orderBy(desc(uptimeIncident.openedAt), desc(uptimeIncident.id));
  const incidents = yield* Effect.forEach(incidentRows, (row) =>
    decodeEvidence(row.openedBy, { onExcessProperty: "error" }).pipe(
      Effect.mapError((cause) => new Error(`Incident ${row.id} has opened_by the dashboard can't read: ${cause}`)),
      Effect.orDie,
      Effect.map((evidence) => ({
        id: row.id,
        openedAt: row.openedAt.toISOString(),
        closedAt: row.closedAt?.toISOString() ?? null,
        observations: evidence.observations,
      })),
    ));

  return { check, status, uptime, days, incidents } satisfies ServiceUptime;
});

/** The Service's latency chart for `range`, ordered by `at`, then region. Empty without a check. */
export const readServiceUptimeSeries = Effect.fn("Uptime.readServiceSeries")(function* (
  actor: Actor,
  input: ReadServiceUptimeSeriesInput,
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const now = new Date(yield* Clock.currentTimeMillis);
  const check = yield* findCheck(organization.id, input);
  if (check === undefined) return [];
  const of = ofCheck(check.id, organization.id);
  const since = (series: "raw" | "hourly") =>
    clampSince(organization.id, series, new Date(now.getTime() - UPTIME_RANGE_MS[input.range]), now);

  if (input.range === "24h") {
    const rows = yield* drizzle
      .select({ at: uptimeResult.scheduledAt, region: uptimeResult.region, verdict: uptimeResult.verdict,
        totalMs: uptimeResult.totalMs })
      .from(uptimeResult)
      .where(and(of(uptimeResult), eq(uptimeResult.kind, "routine"), gte(uptimeResult.scheduledAt, since("raw"))))
      .orderBy(asc(uptimeResult.scheduledAt), asc(uptimeResult.region));
    return rows.map((row): UptimePoint => ({
      at: row.at.toISOString(), region: row.region, ms: row.verdict === "pass" ? row.totalMs : null,
    }));
  }

  if (input.range === "7d") {
    const rows = yield* drizzle
      .select({ at: uptimeResultHourly.hour, region: uptimeResultHourly.region, ms: uptimeResultHourly.passMsP50 })
      .from(uptimeResultHourly)
      .where(and(of(uptimeResultHourly), gte(uptimeResultHourly.hour, since("hourly"))))
      .orderBy(asc(uptimeResultHourly.hour), asc(uptimeResultHourly.region));
    return rows.map((row): UptimePoint => ({ ...row, at: row.at.toISOString() }));
  }

  const passing = sql`(${uptimeResultHourly.probes} - ${uptimeResultHourly.failures})`;
  const weighted = sql`${uptimeResultHourly.passMsP50} IS NOT NULL`;
  const bucket = sql<Date>`to_timestamp(floor(extract(epoch from ${uptimeResultHourly.hour}) / 21600) * 21600)`
    .mapWith(uptimeResultHourly.hour);
  const rows = yield* drizzle
    .select({
      at: bucket,
      region: uptimeResultHourly.region,
      ms: sql<number | null>`round(sum(${uptimeResultHourly.passMsP50} * ${passing}) FILTER (WHERE ${weighted})
        / nullif(sum(${passing}) FILTER (WHERE ${weighted}), 0))`.mapWith(Number),
    })
    .from(uptimeResultHourly)
    .where(and(of(uptimeResultHourly), gte(uptimeResultHourly.hour, since("hourly"))))
    .groupBy(bucket, uptimeResultHourly.region)
    .orderBy(asc(bucket), asc(uptimeResultHourly.region));
  return rows.map((row): UptimePoint => ({ at: row.at.toISOString(), region: row.region, ms: row.ms }));
});
