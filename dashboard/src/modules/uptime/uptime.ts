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

/**
 * The open incident of a check that never passed in its range, when its first failure is a 4xx: the server answers, but not at the
 * healthcheck path. Null otherwise. A closed incident or any counted pass means the path worked once.
 */
export function wrongPathIncident(uptime: ServiceUptime): UptimeIncidentEntry | null {
  const open = openIncident(uptime.incidents);
  const failure = open?.observations.find((observation) => observation.verdict === "fail");
  const neverPassed = !uptime.uptime && uptime.incidents.every((incident) => incident.closedAt === null);
  return open && failure && failure.status >= 400 && failure.status < 500 && neverPassed ? open : null;
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

/**
 * The window a zoomed incident shows: five times its length, at least three hours and 24 steps, centred on it and never
 * past `to`. Its steps are never finer than the series `rangeCovering` reads for it, or the line would break between points.
 * When no range reaches back that far, it fits inside the 90 days, as close to the incident as it gets.
 */
export function incidentWindow(incident: UptimeIncidentEntry, to: number): TimeWindow {
  const span = incidentSpan(incident, to);
  const windowFor = (range: UptimeRange, maxLength = Infinity): TimeWindow => {
    const base = RANGE_STEP_MS[range];
    const length = Math.min(maxLength, Math.max(3 * HOUR_MS, (span.to - span.from) * 5, 24 * base));
    const stepMs = Math.ceil(length / 144 / base) * base;
    const end = Math.min(to, Math.ceil(((span.from + span.to) / 2 + length / 2) / stepMs) * stepMs);
    return { from: end - length, to: end, stepMs };
  };
  const covered = UPTIME_RANGES.find((range) => to - UPTIME_RANGE_MS[range] <= windowFor(range).from);
  if (covered) return windowFor(covered);
  const window = windowFor("90d", UPTIME_RANGE_MS["90d"]);
  const from = Math.max(window.from, to - UPTIME_RANGE_MS["90d"]);
  return { ...window, from, to: from + window.to - window.from };
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

/** Each range's history bar: an hour, 4 hours, a UTC day. */
const BAR_MS = { "24h": HOUR_MS, "7d": 4 * HOUR_MS, "90d": DAY_MS } satisfies Record<UptimeRange, number>;

export const barCount = (range: UptimeRange) => UPTIME_RANGE_MS[range] / BAR_MS[range];

/** One bar of the history strip: the incidents that touched it, and whether the check ran in it. */
export type HistoryBar = { readonly start: number; readonly incidents: readonly UptimeIncidentEntry[]; readonly checked: boolean };

/** `range` up to `to` in bars, oldest first. A bar is checked when a point or an incident falls in it. */
export function historyBars(range: UptimeRange, to: number, uptime: Pick<ServiceUptime, "points" | "incidents"> | null): HistoryBar[] {
  const stepMs = BAR_MS[range];
  const probed = new Set(uptime?.points.map((point) => Math.floor(Date.parse(point.at) / stepMs) * stepMs));
  return steps({ from: to - UPTIME_RANGE_MS[range], to, stepMs }).map((start) => {
    const incidents = (uptime?.incidents ?? []).filter((incident) => {
      const span = incidentSpan(incident, to);
      return span.from < start + stepMs && span.to > start;
    });
    return { start, incidents, checked: probed.has(start) || incidents.length > 0 };
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
