import "@tanstack/react-start/server-only";
import { and, asc, desc, eq, gte, isNull, lte, or, sql } from "drizzle-orm";
import type { PgColumn } from "drizzle-orm/pg-core";
import { Clock, Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { clampSince } from "./retention";
import { uptimeCheck, uptimeIncident, uptimeResult, uptimeResultHourly } from "./tables";
import {
  IncidentEvidence,
  type OpenUptimeIncident,
  type ReadOpenUptimeIncidentsInput,
  type ReadUptimeCheckInput,
  UPTIME_RANGE_MS,
  type UptimeCheckDetail,
  type UptimeRange,
} from "./uptime-status";

/**
 * The environment's checks with an open incident. The incident's composite foreign key keeps its check in its
 * organization, so filtering incidents by the actor's organization hides every other organization's environment.
 */
export const readOpenUptimeIncidents = Effect.fn("Uptime.readOpenIncidents")(function* (
  actor: Actor,
  input: ReadOpenUptimeIncidentsInput,
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({
      checkId: uptimeCheck.id,
      serviceId: uptimeCheck.serviceId,
      incidentId: uptimeIncident.id,
      openedAt: uptimeIncident.openedAt,
    })
    .from(uptimeIncident)
    .innerJoin(uptimeCheck, eq(uptimeCheck.id, uptimeIncident.checkId))
    .where(and(
      eq(uptimeIncident.organizationId, organization.id),
      eq(uptimeCheck.environmentId, input.environmentId),
      isNull(uptimeIncident.closedAt),
    ))
    .orderBy(desc(uptimeIncident.openedAt), asc(uptimeCheck.id));
  return rows.map((row): OpenUptimeIncident => ({ ...row, openedAt: row.openedAt.toISOString() }));
});

const decodeEvidence = Schema.decodeUnknownEffect(IncidentEvidence);

/** One check with its status, uptime, the chart for `range` and the incidents that overlap it. */
export const readUptimeCheck = Effect.fn("Uptime.readCheck")(function* (actor: Actor, input: ReadUptimeCheckInput) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const now = new Date(yield* Clock.currentTimeMillis);
  const since = (series: "raw" | "hourly" | "incidents", range: UptimeRange) =>
    clampSince(organization.id, series, new Date(now.getTime() - UPTIME_RANGE_MS[range]), now);

  const [check] = yield* drizzle
    .select({
      id: uptimeCheck.id,
      name: uptimeCheck.name,
      url: uptimeCheck.url,
      serviceId: uptimeCheck.serviceId,
      intervalSeconds: uptimeCheck.intervalSeconds,
      timeoutMs: uptimeCheck.timeoutMs,
      expect: uptimeCheck.expect,
      enabled: uptimeCheck.enabled,
    })
    .from(uptimeCheck)
    .where(and(eq(uptimeCheck.id, input.checkId), eq(uptimeCheck.organizationId, organization.id)));
  if (check === undefined) return yield* new NotFound({ message: "The uptime check was not found." });
  const ofCheck = (table: { checkId: PgColumn; organizationId: PgColumn }) =>
    and(eq(table.checkId, check.id), eq(table.organizationId, organization.id));

  const [open] = yield* drizzle
    .select({ id: uptimeIncident.id })
    .from(uptimeIncident)
    .where(and(ofCheck(uptimeIncident), isNull(uptimeIncident.closedAt)));
  const [probed] = open !== undefined ? [] : yield* drizzle
    .select({ at: uptimeResult.scheduledAt })
    .from(uptimeResult)
    .where(and(ofCheck(uptimeResult),
      gte(uptimeResult.scheduledAt, clampSince(organization.id, "raw", new Date(0), now))))
    .limit(1);
  const status = open !== undefined ? "unreachable" : probed === undefined ? "pending" : "up";

  const window = (range: UptimeRange) => sql`${uptimeResultHourly.hour} >= ${since("hourly", range).toISOString()}`;
  const passing = (range: UptimeRange) => sql<number | null>`
    sum(${uptimeResultHourly.probes} - ${uptimeResultHourly.failures}) FILTER (WHERE ${window(range)})`.mapWith(Number);
  const counted = (range: UptimeRange) =>
    sql<number | null>`sum(${uptimeResultHourly.probes}) FILTER (WHERE ${window(range)})`.mapWith(Number);
  const [sums] = yield* drizzle
    .select({
      pass24h: passing("24h"), probes24h: counted("24h"),
      pass7d: passing("7d"), probes7d: counted("7d"),
      pass90d: passing("90d"), probes90d: counted("90d"),
    })
    .from(uptimeResultHourly)
    .where(and(ofCheck(uptimeResultHourly), window("90d")));
  const fraction = (pass: number | null | undefined, probes: number | null | undefined) =>
    probes === null || probes === undefined || probes === 0 ? null : (pass ?? 0) / probes;
  const uptime = {
    "24h": fraction(sums?.pass24h, sums?.probes24h),
    "7d": fraction(sums?.pass7d, sums?.probes7d),
    "90d": fraction(sums?.pass90d, sums?.probes90d),
  };

  const series = input.range === "24h"
    ? (yield* drizzle
      .select({ at: uptimeResult.scheduledAt, region: uptimeResult.region, verdict: uptimeResult.verdict,
        totalMs: uptimeResult.totalMs })
      .from(uptimeResult)
      .where(and(ofCheck(uptimeResult), eq(uptimeResult.kind, "routine"),
        gte(uptimeResult.scheduledAt, since("raw", "24h"))))
      .orderBy(asc(uptimeResult.scheduledAt), asc(uptimeResult.region)))
      .map((row) => ({ at: row.at.toISOString(), region: row.region, ms: row.verdict === "pass" ? row.totalMs : null }))
    : (yield* drizzle
      .select({ at: uptimeResultHourly.hour, region: uptimeResultHourly.region, ms: uptimeResultHourly.passMsP50 })
      .from(uptimeResultHourly)
      .where(and(ofCheck(uptimeResultHourly), window(input.range)))
      .orderBy(asc(uptimeResultHourly.hour), asc(uptimeResultHourly.region)))
      .map((row) => ({ ...row, at: row.at.toISOString() }));

  const incidentRows = yield* drizzle
    .select({
      id: uptimeIncident.id,
      openedAt: uptimeIncident.openedAt,
      closedAt: uptimeIncident.closedAt,
      openedBy: uptimeIncident.openedBy,
    })
    .from(uptimeIncident)
    .where(and(
      ofCheck(uptimeIncident),
      lte(uptimeIncident.openedAt, now),
      or(isNull(uptimeIncident.closedAt), gte(uptimeIncident.closedAt, since("incidents", input.range))),
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

  return { check, status, uptime, range: input.range, series, incidents } satisfies UptimeCheckDetail;
});
