import "@tanstack/react-start/server-only";
import { sql, type SQL } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { failureClassSql, failureColumn, failureColumns, failureTextSql } from "./failure-sql.server";
import { PRUNE_BATCH_ROWS, type Retained, retentionCutoff } from "./retention";
import { FAILURE_CLASSES } from "./uptime-status";

const HOUR_MS = 3_600_000;

/**
 * The hours a rollup at `now` recomputes: the two closed hours before the current one. Every closed hour is
 * recomputed by two runs, so a result that lands up to an hour late still counts. The current hour is never rolled up.
 */
export function rollupHours(now: Date): readonly [Date, Date] {
  const current = Math.floor(now.getTime() / HOUR_MS) * HOUR_MS;
  return [new Date(current - 2 * HOUR_MS), new Date(current - HOUR_MS)];
}

/**
 * Recompute one hour of uptime_result_hourly from raw routine and triggered probes, per (check, region). It overwrites the bucket,
 * so a rerun writes the same values. `probes` is pass plus fail. Inconclusive is counted apart and never fails. The
 * timings are over passing probes only, and the median is null when none passed. Each failure counts once, in its
 * class, and the newest is kept as `last_error`.
 */
const rollupHourStatement = (hour: Date) => sql`
  WITH probes AS (
    SELECT DISTINCT ON (check_id, region, scheduled_at, kind) check_id, organization_id, region, scheduled_at, verdict,
           status, detail, total_ms, dns_ms, connect_ms, tls_ms, ttfb_ms,
           CASE WHEN verdict = 'fail' THEN ${failureClassSql} END AS cls
    FROM uptime_result
    WHERE kind IN ('routine', 'triggered') AND scheduled_at >= ${hour.toISOString()}::timestamptz
      AND scheduled_at < ${hour.toISOString()}::timestamptz + interval '1 hour'
    ORDER BY check_id, region, scheduled_at, kind),
  newest_failure AS (
    SELECT DISTINCT ON (check_id, region) check_id, region, cls, ${failureTextSql(sql`cls`)} AS text, scheduled_at AS at
    FROM probes WHERE cls IS NOT NULL
    ORDER BY check_id, region, scheduled_at DESC)
  INSERT INTO uptime_result_hourly AS h (check_id, organization_id, region, hour, probes, failures, inconclusive,
                                         pass_ms_sum, pass_ms_max, pass_ms_p50,
                                         dns_ms_sum, connect_ms_sum, tls_ms_sum, ttfb_ms_sum, ${failureColumns},
                                         last_error, last_error_class, last_error_at)
  SELECT p.check_id, p.organization_id, p.region, ${hour.toISOString()}::timestamptz,
         count(*) FILTER (WHERE verdict <> 'inconclusive'), count(*) FILTER (WHERE verdict = 'fail'),
         count(*) FILTER (WHERE verdict = 'inconclusive'),
         coalesce(sum(total_ms) FILTER (WHERE verdict = 'pass'), 0),
         coalesce(max(total_ms) FILTER (WHERE verdict = 'pass'), 0),
         (percentile_cont(0.5) WITHIN GROUP (ORDER BY total_ms) FILTER (WHERE verdict = 'pass'))::int,
         coalesce(sum(dns_ms) FILTER (WHERE verdict = 'pass'), 0), coalesce(sum(connect_ms) FILTER (WHERE verdict = 'pass'), 0),
         coalesce(sum(tls_ms) FILTER (WHERE verdict = 'pass'), 0), coalesce(sum(ttfb_ms) FILTER (WHERE verdict = 'pass'), 0),
         ${sql.join(FAILURE_CLASSES.map((cls) => sql`count(*) FILTER (WHERE p.cls = ${cls})`), sql`, `)},
         f.text, f.cls, f.at
  FROM probes p LEFT JOIN newest_failure f ON f.check_id = p.check_id AND f.region = p.region
  GROUP BY p.check_id, p.organization_id, p.region, f.text, f.cls, f.at
  ON CONFLICT (check_id, region, hour) DO UPDATE SET
    probes = excluded.probes, failures = excluded.failures, inconclusive = excluded.inconclusive,
    pass_ms_sum = excluded.pass_ms_sum, pass_ms_max = excluded.pass_ms_max, pass_ms_p50 = excluded.pass_ms_p50,
    dns_ms_sum = excluded.dns_ms_sum, connect_ms_sum = excluded.connect_ms_sum, tls_ms_sum = excluded.tls_ms_sum,
    ttfb_ms_sum = excluded.ttfb_ms_sum,
    ${sql.raw(FAILURE_CLASSES.map((cls) => `${failureColumn(cls)} = excluded.${failureColumn(cls)}`).join(", "))},
    last_error = excluded.last_error, last_error_class = excluded.last_error_class, last_error_at = excluded.last_error_at`;

/** `uptime-rollup`: recompute the two closed hours before `now`, one statement each. */
export const rollupUptime = Effect.fn("Uptime.rollup")(function* (now: Date) {
  const { drizzle } = yield* Database;
  for (const hour of rollupHours(now)) yield* drizzle.execute(rollupHourStatement(hour));
});

/** One batch of rows older than `before`, at most PRUNE_BATCH_ROWS, as one statement. */
const pruneStatements = {
  raw: (before) => sql`
    DELETE FROM uptime_result WHERE ctid = ANY(ARRAY(
      SELECT ctid FROM uptime_result WHERE scheduled_at < ${before}::timestamptz LIMIT ${PRUNE_BATCH_ROWS}))`,
  hourly: (before) => sql`
    DELETE FROM uptime_result_hourly WHERE ctid = ANY(ARRAY(
      SELECT ctid FROM uptime_result_hourly WHERE hour < ${before}::timestamptz LIMIT ${PRUNE_BATCH_ROWS}))`,
  // Their alerts go with them by cascade.
  incidents: (before) => sql`
    DELETE FROM uptime_incident WHERE id IN (
      SELECT id FROM uptime_incident WHERE closed_at < ${before}::timestamptz LIMIT ${PRUNE_BATCH_ROWS})`,
  finishedAlerts: (before) => sql`
    DELETE FROM uptime_alert WHERE id IN (
      SELECT id FROM uptime_alert
      WHERE state IN ('sent', 'failed', 'expired') AND updated_at < ${before}::timestamptz LIMIT ${PRUNE_BATCH_ROWS})`,
} satisfies Record<Retained, (before: string) => SQL>;

export const prunedKinds =
  // SAFETY: pruneStatements is a literal checked by `satisfies`, so its own keys are exactly the Retained kinds.
  Object.keys(pruneStatements) as ReadonlyArray<Retained>;

/** `uptime-prune`: delete one batch of `kind` past its retention at `now`. Returns the rows deleted. */
export const pruneUptimeBatch = Effect.fn("Uptime.prune")(function* (kind: Retained, now: Date) {
  const { drizzle } = yield* Database;
  const [counted] = yield* drizzle.execute<{ deleted: number }>(sql`
    WITH gone AS (${pruneStatements[kind](retentionCutoff(kind, now).toISOString())} RETURNING 1)
    SELECT count(*)::int AS deleted FROM gone`, "objects");
  return counted?.deleted ?? 0;
});
