import "@tanstack/react-start/server-only";
import { and, asc, between, desc, eq, exists, gte, inArray, isNull, lt, lte, or, sql } from "drizzle-orm";
import type { PgColumn } from "drizzle-orm/pg-core";
import { Clock, Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { Database } from "#/server/database.server";
import { failureClassSql, failureColumn, failureTextSql, phaseColumn } from "./failure-sql.server";
import { clampSince, type UptimeSeries } from "./retention";
import { uptimeCheck, uptimeIncident, uptimeRegion, uptimeResult, uptimeResultHourly } from "./tables";
import {
  type EnvironmentUptimeCheck,
  IncidentEvidence,
  type ReadEnvironmentUptimeInput,
  type ReadServiceUptimeInput,
  type RequestUptimeProbeInput,
  type ServiceUptime,
  FAILURE_CLASSES,
  type FailureClass,
  PHASES,
  type UptimeCheckResult,
  type UptimePoint,
} from "./uptime-status";
import { tally } from "./uptime";
import { checkWindow, MAX_CHECK_BARS, perCheck, type PointSource, pointSource, type RangeWindow, resolveRange } from "./uptime-range";

const HOUR_MS = 3_600_000;
// Long enough for a self-host round, whose second probe goes 30 s after the first, to land and move the status.
const PROBE_SHOWN_MS = 45_000;

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
    .select({
      id: uptimeCheck.id, url: uptimeCheck.url, intervalSeconds: uptimeCheck.intervalSeconds,
      probeRequestedAt: uptimeCheck.probeRequestedAt,
    })
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

/** The probes uptime and the chart count. A triggered probe is evidence like a routine one; confirms and rechecks only decide incidents. */
const countedKinds = inArray(uptimeResult.kind, ["routine", "triggered"]);

/** The Service's check with its status, uptime, latency points, results and the incidents over a range. Null without a check. */
export const readServiceUptime = Effect.fn("Uptime.readService")(function* (actor: Actor, input: ReadServiceUptimeInput) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const now = new Date(yield* Clock.currentTimeMillis);
  const found = yield* findCheck(organization.id, input);
  if (found === undefined) return null;
  const { probeRequestedAt, ...check } = found;
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

  // The hourly rollup counts the whole hours that end before raw retention's next hour and by `to`; raw probes count the rest.
  const rawFrom = since("raw", checked.from);
  const split = rawFrom.getTime() === checked.from ? rawFrom : new Date(Math.ceil(rawFrom.getTime() / HOUR_MS) * HOUR_MS);
  const hoursEnd = Math.floor(Math.min(split.getTime(), checked.to) / HOUR_MS) * HOUR_MS;
  const rawStart = new Date(Math.max(rawFrom.getTime(), hoursEnd));
  const [hourly] = split.getTime() === checked.from ? [] : yield* drizzle
    .select({
      passing: sql<number | null>`sum(${uptimeResultHourly.probes} - ${uptimeResultHourly.failures})`.mapWith(Number),
      counted: sql<number | null>`sum(${uptimeResultHourly.probes})`.mapWith(Number),
    })
    .from(uptimeResultHourly)
    .where(and(
      of(uptimeResultHourly),
      gte(uptimeResultHourly.hour, since("hourly", checked.from)),
      lte(uptimeResultHourly.hour, new Date(hoursEnd - HOUR_MS)),
    ));
  const [raw] = yield* drizzle
    .select({
      passing: sql<number>`count(*) FILTER (WHERE ${uptimeResult.verdict} = 'pass')`.mapWith(Number),
      counted: sql<number>`count(*) FILTER (WHERE ${uptimeResult.verdict} IN ('pass', 'fail'))`.mapWith(Number),
    })
    .from(uptimeResult)
    .where(and(of(uptimeResult), countedKinds, between(uptimeResult.scheduledAt, rawStart, to)));
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
  const codes = [...new Set([
    ...points.map((point) => point.region),
    ...(checks ?? []).map((result) => result.region),
    ...incidents.flatMap((incident) => incident.observations.map((observation) => observation.region)),
  ])];
  const named = yield* drizzle.select({ region: uptimeRegion.region, name: uptimeRegion.name }).from(uptimeRegion);
  const regions = { ...Object.fromEntries(codes.map((code) => [code, null])), ...Object.fromEntries(named.map((row) => [row.region, row.name])) };
  const probe = probeRequestedAt === null || now.getTime() - probeRequestedAt.getTime() > PROBE_SHOWN_MS ? null : {
    requestedAt: probeRequestedAt.toISOString(),
    answered: (yield* drizzle
      .select({ at: uptimeResult.scheduledAt })
      .from(uptimeResult)
      .where(and(of(uptimeResult), eq(uptimeResult.kind, "triggered"), gte(uptimeResult.scheduledAt, probeRequestedAt)))
      .limit(1)).length > 0,
  };
  return { check, status, uptime, points, checks, incidents, regions, probe } satisfies ServiceUptime;
});

/**
 * Ask the checker to probe the Service's check now. A check asked within the last 15 s and not yet answered is left
 * alone, so a burst of clicks writes once while a click after the answer asks again. A Service without a check writes
 * nothing.
 */
export const requestUptimeProbe = Effect.fn("Uptime.requestProbe")(function* (actor: Actor, input: RequestUptimeProbeInput) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const answered = exists(drizzle.select({ one: sql`1` }).from(uptimeResult).where(and(
    eq(uptimeResult.checkId, uptimeCheck.id),
    eq(uptimeResult.organizationId, uptimeCheck.organizationId),
    eq(uptimeResult.kind, "triggered"),
    gte(uptimeResult.scheduledAt, uptimeCheck.probeRequestedAt),
  )));
  yield* drizzle.update(uptimeCheck).set({ probeRequestedAt: sql`now()` }).where(and(
    eq(uptimeCheck.organizationId, organization.id),
    eq(uptimeCheck.environmentId, input.environmentId),
    eq(uptimeCheck.serviceId, input.serviceId),
    or(isNull(uptimeCheck.probeRequestedAt), lt(uptimeCheck.probeRequestedAt, sql`now() - interval '15 seconds'`), answered),
  ));
  return null;
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

/** Every routine and triggered probe between `from` and `to`, each as its own point. */
const readRawPoints = Effect.fn("Uptime.readRawPoints")(function* (of: ReturnType<typeof ofCheck>, from: Date, to: Date) {
  const { drizzle } = yield* Database;
  const failed = sql`${uptimeResult.verdict} = 'fail'`;
  const rows = yield* drizzle
    .select({
      at: uptimeResult.scheduledAt, region: uptimeResult.region, verdict: uptimeResult.verdict,
      totalMs: uptimeResult.totalMs, dns: uptimeResult.dnsMs, connect: uptimeResult.connectMs, tls: uptimeResult.tlsMs,
      ttfb: uptimeResult.ttfbMs,
      cls: sql<FailureClass | null>`CASE WHEN ${failed} THEN ${failureClassSql} END`,
      text: sql<string | null>`CASE WHEN ${failed} THEN ${failureTextSql(failureClassSql)} END`,
    })
    .from(uptimeResult)
    .where(and(of(uptimeResult), countedKinds, between(uptimeResult.scheduledAt, from, to)))
    .orderBy(asc(uptimeResult.scheduledAt), asc(uptimeResult.region));
  return rows.map((row): UptimePoint => {
    const at = row.at.toISOString();
    const passed = row.verdict === "pass";
    return {
      at,
      region: row.region,
      probes: row.verdict === "inconclusive" ? 0 : 1,
      failures: tally(FAILURE_CLASSES, (cls) => row.cls === cls ? 1 : 0),
      medianMs: passed ? row.totalMs : null,
      maxMs: passed ? row.totalMs : null,
      phaseMsSum: tally(PHASES, (phase) => passed ? row[phase] : 0),
      lastError: row.cls === null || row.text === null ? null : { at, cls: row.cls, text: row.text },
    };
  });
});

type HourlyRow = Record<ReturnType<typeof failureColumn> | ReturnType<typeof phaseColumn> | "probes", number> & {
  at: Date; region: string; medianMs: number | null; maxMs: number | null;
  lastError: string | null; lastErrorClass: FailureClass | null; lastErrorAt: Date | null;
};

/**
 * The latency chart over `window`, ordered by `at`, then region. A rollup source reads the hourly rollup, then raw probes
 * from the first hour it has not rolled up, so a check's newest probes show before :05 of the next hour. A sixHourly
 * source merges each region's hours into 6-hour points by `mergePoints`' rule.
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
  const bucket = source === "hourly" ? sql`hour` : sql`to_timestamp(floor(extract(epoch from hour) / 21600) * 21600)`;
  const weighted = sql`FILTER (WHERE pass_ms_p50 IS NOT NULL)`;
  const newest = (column: string) =>
    sql`(array_agg(${sql.raw(column)} ORDER BY last_error_at DESC) FILTER (WHERE last_error_at IS NOT NULL))[1]`;
  const rows = yield* drizzle.execute<HourlyRow>(sql`
    SELECT ${bucket} AS at, region, sum(probes)::int AS probes,
           ${sql.join(FAILURE_CLASSES.map((cls) => sql`sum(${sql.raw(failureColumn(cls))})::int AS ${sql.raw(failureColumn(cls))}`), sql`, `)},
           sum(pass_ms_p50::float8 * (probes - failures)) ${weighted} / nullif(sum(probes - failures) ${weighted}, 0) AS "medianMs",
           max(pass_ms_max) FILTER (WHERE probes > failures) AS "maxMs",
           ${sql.join(PHASES.map((phase) => sql`sum(${sql.raw(phaseColumn(phase))})::float8 AS ${sql.raw(phaseColumn(phase))}`), sql`, `)},
           ${newest("last_error")} AS "lastError", ${newest("last_error_class")} AS "lastErrorClass",
           max(last_error_at) AS "lastErrorAt"
    FROM uptime_result_hourly
    WHERE ${of(uptimeResultHourly)}
      AND hour BETWEEN ${hourlySince.toISOString()}::timestamptz AND ${to.toISOString()}::timestamptz
    GROUP BY 1, region
    ORDER BY 1, region`, "objects");
  return [...rows.map((row): UptimePoint => ({
    at: new Date(row.at).toISOString(),
    region: row.region,
    probes: row.probes,
    failures: tally(FAILURE_CLASSES, (cls) => row[failureColumn(cls)]),
    medianMs: row.medianMs,
    maxMs: row.maxMs,
    phaseMsSum: tally(PHASES, (phase) => row[phaseColumn(phase)]),
    lastError: row.lastError === null || row.lastErrorClass === null || row.lastErrorAt === null ? null
      : { at: new Date(row.lastErrorAt).toISOString(), cls: row.lastErrorClass, text: row.lastError },
  })), ...tail];
});
