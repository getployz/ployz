/**
 * Every uptime retention number, in one place. `uptime-prune` deletes what is older, and every read clamps its range
 * with `clampSince`, so a read never shows a row the prune is about to take.
 */
export const UPTIME_RETENTION = {
  /** uptime_result, by scheduled_at. */
  raw: { days: 30 },
  /** uptime_result_hourly, by hour. */
  hourly: { months: 13 },
  /** Closed uptime_incident rows, by closed_at. */
  incidents: { months: 13 },
  /** sent, failed and expired uptime_alert rows, by updated_at. */
  finishedAlerts: { days: 30 },
} as const satisfies Record<string, { days: number } | { months: number }>;

export type Retained = keyof typeof UPTIME_RETENTION;
/** The history a dashboard read can ask for. */
export type UptimeSeries = Exclude<Retained, "finishedAlerts">;

/** Rows per prune statement, so no delete holds a long transaction. */
export const PRUNE_BATCH_ROWS = 5000;

/** Calendar months back in UTC, with the day clamped to the month's end, as Postgres `- interval 'N months'` does. */
function monthsBefore(at: Date, months: number) {
  const shifted = new Date(at);
  shifted.setUTCDate(1);
  shifted.setUTCMonth(shifted.getUTCMonth() - months);
  const lastDay = new Date(Date.UTC(shifted.getUTCFullYear(), shifted.getUTCMonth() + 1, 0)).getUTCDate();
  shifted.setUTCDate(Math.min(at.getUTCDate(), lastDay));
  return shifted;
}

/** The oldest instant `kind` keeps at `now`. Rows strictly older are pruned. */
export function retentionCutoff(kind: Retained, now: Date): Date {
  const retention: { days: number } | { months: number } = UPTIME_RETENTION[kind];
  return "days" in retention
    ? new Date(now.getTime() - retention.days * 86_400_000)
    : monthsBefore(now, retention.months);
}

/**
 * The visible start of a read: `since`, moved up to the series' retention cutoff when it reaches further back. Every
 * uptime read applies it. Retention is the same for every organization today. A per-plan cutoff would read the
 * organization here.
 */
export function clampSince(_organizationId: string, series: UptimeSeries, since: Date, now: Date): Date {
  const cutoff = retentionCutoff(series, now);
  return since < cutoff ? cutoff : since;
}
