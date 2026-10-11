import "@tanstack/react-start/server-only";
import { and, asc, between, desc, eq, gte, isNull, lte, or, sql } from "drizzle-orm";
import type { PgColumn } from "drizzle-orm/pg-core";
import { Clock, Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { Database } from "#/server/database.server";
import { clampSince, type UptimeSeries } from "./retention";
import { uptimeCheck, uptimeIncident, uptimeResult, uptimeResultHourly } from "./tables";
import {
  type EnvironmentUptimeCheck,
  IncidentEvidence,
  type ReadEnvironmentUptimeInput,
  type ReadServiceUptimeInput,
  type ServiceUptime,
  type UptimeCheckResult,
  type UptimePoint,
} from "./uptime-status";
import { checkWindow, MAX_CHECK_BARS, perCheck, type PointSource, pointSource, type RangeWindow, resolveRange } from "./uptime-range";

const HOUR_MS = 3_600_000;

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

/** The Service's check with its status, uptime, latency points, results and the incidents over a range. Null without a check. */
export const readServiceUptime = Effect.fn("Uptime.readService")(function* (actor: Actor, input: ReadServiceUptimeInput) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const now = new Date(yield* Clock.currentTimeMillis);
  const check = yield* findCheck(organization.id, input);
  if (check === undefined) return null;
  const of = ofCheck(check.id, organization.id);
  const window = resolveRange(input, now.getTime());
  const checked = checkWindow(window);
  const since = (series: UptimeSeries, from: number) => clampSince(organization.id, series, new Date(from), now);
  const to = new Date(checked.to);

  const [open] = yield* drizzle
    .select({ id: uptimeIncident.id })
    .from(uptimeIncident)
    .where(and(of(uptimeIncident), isNull(uptimeIncident.closedAt)));
  const [probed] = open !== undefined ? [] : yield* drizzle
    .select({ at: uptimeResult.scheduledAt })
    .from(uptimeResult)
    .where(and(of(uptimeResult), gte(uptimeResult.scheduledAt, since("raw", 0))))
    .limit(1);
  const status = open !== undefined ? "unreachable" : probed === undefined ? "pending" : "up";

  // Raw probes count where raw retention reaches; the hourly rollup counts the whole hours before that and before `to`.
  const rawFrom = since("raw", checked.from);
  const split = rawFrom.getTime() === checked.from ? rawFrom : new Date(Math.ceil(rawFrom.getTime() / HOUR_MS) * HOUR_MS);
  const [hourly] = split.getTime() === checked.from ? [] : yield* drizzle
    .select({
      passing: sql<number | null>`sum(${uptimeResultHourly.probes} - ${uptimeResultHourly.failures})`.mapWith(Number),
      counted: sql<number | null>`sum(${uptimeResultHourly.probes})`.mapWith(Number),
    })
    .from(uptimeResultHourly)
    .where(and(
      of(uptimeResultHourly),
      gte(uptimeResultHourly.hour, since("hourly", checked.from)),
      lte(uptimeResultHourly.hour, new Date(Math.min(split.getTime(), checked.to) - HOUR_MS)),
    ));
  const [raw] = yield* drizzle
    .select({
      passing: sql<number>`count(*) FILTER (WHERE ${uptimeResult.verdict} = 'pass')`.mapWith(Number),
      counted: sql<number>`count(*) FILTER (WHERE ${uptimeResult.verdict} IN ('pass', 'fail'))`.mapWith(Number),
    })
    .from(uptimeResult)
    .where(and(of(uptimeResult), eq(uptimeResult.kind, "routine"), between(uptimeResult.scheduledAt, split, to)));
  const counted = (hourly?.counted ?? 0) + (raw?.counted ?? 0);
  const uptime = counted === 0 ? null : ((hourly?.passing ?? 0) + (raw?.passing ?? 0)) / counted;

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
      lte(uptimeIncident.openedAt, to),
      or(isNull(uptimeIncident.closedAt), gte(uptimeIncident.closedAt, since("incidents", checked.from))),
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

  const checks = perCheck(checked) && rawFrom.getTime() === checked.from ? yield* readChecks(of, rawFrom, to) : null;
  const points = yield* readPoints(of, window, pointSource(window, now.getTime()), since);
  return { check, status, uptime, points, checks, incidents } satisfies ServiceUptime;
});

/** The newest results of every kind between `from` and `to`, oldest first. */
const readChecks = Effect.fn("Uptime.readChecks")(function* (of: ReturnType<typeof ofCheck>, from: Date, to: Date) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ at: uptimeResult.scheduledAt, region: uptimeResult.region, kind: uptimeResult.kind,
      verdict: uptimeResult.verdict, status: uptimeResult.status, ms: uptimeResult.totalMs })
    .from(uptimeResult)
    .where(and(of(uptimeResult), between(uptimeResult.scheduledAt, from, to)))
    .orderBy(desc(uptimeResult.scheduledAt), desc(uptimeResult.region), desc(uptimeResult.kind))
    .limit(MAX_CHECK_BARS);
  return rows.reverse().map((row): UptimeCheckResult => ({ ...row, at: row.at.toISOString() }));
});

/** Every routine probe between `from` and `to`, with its total time when it passed. */
const readRawPoints = Effect.fn("Uptime.readRawPoints")(function* (of: ReturnType<typeof ofCheck>, from: Date, to: Date) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ at: uptimeResult.scheduledAt, region: uptimeResult.region, verdict: uptimeResult.verdict,
      totalMs: uptimeResult.totalMs })
    .from(uptimeResult)
    .where(and(of(uptimeResult), eq(uptimeResult.kind, "routine"), between(uptimeResult.scheduledAt, from, to)))
    .orderBy(asc(uptimeResult.scheduledAt), asc(uptimeResult.region));
  return rows.map((row): UptimePoint => ({
    at: row.at.toISOString(), region: row.region, ms: row.verdict === "pass" ? row.totalMs : null,
  }));
});

/**
 * The latency chart over `window`, ordered by `at`, then region. A rollup source reads the hourly rollup, then raw probes
 * from the first hour it has not rolled up, so a check's newest probes show before :05 of the next hour.
 */
const readPoints = Effect.fn("Uptime.readPoints")(function* (
  of: ReturnType<typeof ofCheck>,
  window: RangeWindow,
  source: PointSource,
  since: (series: UptimeSeries, from: number) => Date,
) {
  const to = new Date(window.to);
  if (source === "raw") return yield* readRawPoints(of, since("raw", window.from), to);
  const { drizzle } = yield* Database;
  const [rolled] = yield* drizzle
    .select({ hour: sql<Date | null>`max(${uptimeResultHourly.hour})`.mapWith(uptimeResultHourly.hour) })
    .from(uptimeResultHourly)
    .where(of(uptimeResultHourly));
  const tailFrom = new Date(Math.max((rolled?.hour?.getTime() ?? 0) + HOUR_MS, since("raw", window.from).getTime()));
  const tail = tailFrom > to ? [] : yield* readRawPoints(of, tailFrom, to);
  const hourlySince = since("hourly", Math.floor(window.from / HOUR_MS) * HOUR_MS);
  const inWindow = and(of(uptimeResultHourly), between(uptimeResultHourly.hour, hourlySince, to));

  if (source === "hourly") {
    const rows = yield* drizzle
      .select({ at: uptimeResultHourly.hour, region: uptimeResultHourly.region, ms: uptimeResultHourly.passMsP50 })
      .from(uptimeResultHourly)
      .where(inWindow)
      .orderBy(asc(uptimeResultHourly.hour), asc(uptimeResultHourly.region));
    return [...rows.map((row): UptimePoint => ({ ...row, at: row.at.toISOString() })), ...tail];
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
    .where(inWindow)
    .groupBy(bucket, uptimeResultHourly.region)
    .orderBy(asc(bucket), asc(uptimeResultHourly.region));
  return [...rows.map((row): UptimePoint => ({ at: row.at.toISOString(), region: row.region, ms: row.ms })), ...tail];
});
