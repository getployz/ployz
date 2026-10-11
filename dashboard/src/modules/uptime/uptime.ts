import {
  type EnvironmentUptimeCheck,
  type IncidentObservation,
  type ServiceUptime,
  UPTIME_RANGES,
  UPTIME_RANGE_MS,
  type UptimeIncidentEntry,
  type UptimePoint,
  type UptimeRange,
  type UptimeStatus,
} from "./uptime-status";

/** A Service without a check has no public domain. */
export type CheckState = "no_domain" | UptimeStatus;

export const checkState = (uptime: ServiceUptime | null): CheckState => uptime === null ? "no_domain" : uptime.status;

export const openIncident = (incidents: readonly UptimeIncidentEntry[]) => incidents.find((incident) => incident.closedAt === null) ?? null;

/** The hostname whose open incident adds a red ⚠ to its Service's card, or null. */
export function unreachableHost(checks: readonly EnvironmentUptimeCheck[], serviceId: string): string | null {
  const check = checks.find((candidate) => candidate.serviceId === serviceId);
  return check && check.openIncidentAt !== null ? new URL(check.url).hostname : null;
}

/** "99.98%": floored, so a failure never reads as 100%. Null when no probe counted. */
export const uptimePercent = (fraction: number | null) => fraction === null ? null : `${Math.floor(fraction * 10_000) / 100}%`;

const PHASE_WORDS = { dns: "DNS", connect: "Connect", tls: "TLS" } as const;

/** An observation's answer in one word: "502", "Timeout", "Refused", "DNS". */
export function observationText(observation: IncidentObservation): string {
  if (observation.status !== 0) return String(observation.status);
  if (observation.error === "timed_out") return "Timeout";
  if (observation.error === "connection_refused") return "Refused";
  return observation.failedPhase !== null && observation.failedPhase !== "request" ? PHASE_WORDS[observation.failedPhase] : "Error";
}

/** An incident's cause: its first failing observation's answer. */
export function causeText(incident: UptimeIncidentEntry): string {
  const failure = incident.observations.find((observation) => observation.verdict === "fail");
  return failure ? observationText(failure) : "Unknown";
}

const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;
const FIVE_MINUTES = 5 * 60_000;

/** A stretch of time cut into equal steps; every chart on the tab shares one, so a single cursor lines them up. */
export type TimeWindow = { readonly from: number; readonly to: number; readonly stepMs: number };

/** Each range's step matches its series: a 5-minute probe, an hour, 6 hours. */
const RANGE_STEP_MS = { "24h": FIVE_MINUTES, "7d": HOUR_MS, "90d": 6 * HOUR_MS } satisfies Record<UptimeRange, number>;

export const rangeWindow = (to: number, range: UptimeRange): TimeWindow =>
  ({ from: to - UPTIME_RANGE_MS[range], to, stepMs: RANGE_STEP_MS[range] });

/** The shortest range that reaches back to `from`, so a zoom reads the finest series that covers it. */
export const rangeCovering = (from: number, to: number): UptimeRange =>
  UPTIME_RANGES.find((range) => to - UPTIME_RANGE_MS[range] <= from) ?? "90d";

/** An incident's span; an open one runs to `to`. */
export const incidentSpan = (incident: UptimeIncidentEntry, to: number) =>
  ({ from: Date.parse(incident.openedAt), to: incident.closedAt === null ? to : Date.parse(incident.closedAt) });

/** The window a zoomed incident shows: five times its length, at least three hours, centred on it and never past `to`. */
export function incidentWindow(incident: UptimeIncidentEntry, to: number): TimeWindow {
  const span = incidentSpan(incident, to);
  const length = Math.max(3 * HOUR_MS, (span.to - span.from) * 5);
  const end = Math.min(to, Math.ceil(((span.from + span.to) / 2 + length / 2) / FIVE_MINUTES) * FIVE_MINUTES);
  return { from: end - length, to: end, stepMs: Math.max(FIVE_MINUTES, Math.ceil(length / 144 / FIVE_MINUTES) * FIVE_MINUTES) };
}

/** The start of each step in a window. */
export function steps(window: TimeWindow): number[] {
  const first = Math.ceil(window.from / window.stepMs) * window.stepMs;
  return Array.from({ length: Math.max(0, Math.ceil((window.to - first) / window.stepMs)) }, (_, index) => first + index * window.stepMs);
}

/** Response time at each step: the mean of every region's points in it. Null where none passed, so the line breaks. */
export function checkSeries(points: readonly UptimePoint[], window: TimeWindow): (number | null)[] {
  const buckets = new Map<number, { count: number; sum: number }>();
  for (const point of points) {
    if (point.ms === null) continue;
    const step = Math.floor(Date.parse(point.at) / window.stepMs) * window.stepMs;
    const bucket = buckets.get(step) ?? { count: 0, sum: 0 };
    buckets.set(step, { count: bucket.count + 1, sum: bucket.sum + point.ms });
  }
  return steps(window).map((step) => {
    const bucket = buckets.get(step);
    return bucket ? bucket.sum / bucket.count : null;
  });
}

/** Ninety unchecked UTC days up to `to`, oldest first: the strip of a Service without a check. */
export const uncheckedDays = (to: number) => {
  const today = Math.floor(to / DAY_MS) * DAY_MS;
  return Array.from({ length: 90 }, (_, index) => ({ start: new Date(today - (89 - index) * DAY_MS).toISOString(), checked: false }));
};

/** One day of the history strip: the incidents that touched it, and whether the check ran at all. */
export type HistoryDay = { readonly start: number; readonly incidents: readonly UptimeIncidentEntry[]; readonly checked: boolean };

/** The server's 90 days, each with the incidents that touched it. `to` closes an open incident. */
export function historyDays(uptime: Pick<ServiceUptime, "days" | "incidents">, to: number): HistoryDay[] {
  return uptime.days.map((day) => {
    const start = Date.parse(day.start);
    const incidents = uptime.incidents.filter((incident) => {
      const span = incidentSpan(incident, to);
      return span.from < start + DAY_MS && span.to > start;
    });
    return { start, incidents, checked: day.checked || incidents.length > 0 };
  });
}

export const formatMs = (ms: number) => ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(2)} s`;

/** "18m", "2h 5m", "3d 4h". */
export function formatSpan(ms: number) {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`;
  return hours % 24 ? `${Math.floor(hours / 24)}d ${hours % 24}h` : `${Math.floor(hours / 24)}d`;
}
