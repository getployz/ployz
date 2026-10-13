import { type PointSource, pointSource, type RangeWindow } from "./uptime-range";
import {
  type EnvironmentUptimeCheck,
  FAILURE_CLASSES,
  FAILURE_WORDS,
  type FailureClass,
  type FailureCounts,
  type IncidentObservation,
  PHASES,
  type RegionNames,
  type ServiceUptime,
  type UptimeCheckResult,
  type UptimeIncidentEntry,
  type UptimePoint,
  type UptimeStatus,
} from "./uptime-status";

/**
 * What to call a probe region: its display name, else its code, which reads as code. Null on an install with one region, since
 * a label that never changes says nothing.
 */
export function regionLabel(regions: RegionNames, code: string): { readonly text: string; readonly named: boolean } | null {
  if (Object.keys(regions).length <= 1) return null;
  const name = regions[code] ?? null;
  return name === null ? { text: code, named: false } : { text: name, named: true };
}

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

/** The FAILURE_CLASSES a failing observation lands in. */
export function failureClass(observation: Pick<IncidentObservation, "status" | "error" | "failedPhase">): FailureClass {
  if (observation.status !== 0) return "status";
  if (observation.error === "timed_out") return "timeout";
  if (observation.error === "connection_refused") return "refused";
  const phase = observation.failedPhase;
  return phase === "dns" || phase === "connect" || phase === "tls" ? phase : "other";
}

/** An observation's answer in one word: "502", "Timeout", "Refused", "DNS". */
export function observationText(observation: IncidentObservation): string {
  const cls = failureClass(observation);
  return cls === "status" ? String(observation.status) : FAILURE_WORDS[cls];
}

/** An incident's cause: its first failing observation's answer. */
export function causeText(incident: UptimeIncidentEntry): string {
  const failure = incident.observations.find((observation) => observation.verdict === "fail");
  return failure ? observationText(failure) : "Unknown";
}

/**
 * The open incident of a check that never passed in its range, when its first failure is a 4xx. Only a check with a
 * healthcheck path fails on a 4xx, so the app answers that path with it. Null otherwise. A closed incident or any counted
 * pass means the path worked once.
 */
export function wrongPathIncident(uptime: ServiceUptime): UptimeIncidentEntry | null {
  const open = openIncident(uptime.incidents);
  const failure = open?.observations.find((observation) => observation.verdict === "fail");
  const neverPassed = !uptime.uptime && uptime.incidents.every((incident) => incident.closedAt === null);
  return open && failure && failure.status >= 400 && failure.status < 500 && neverPassed ? open : null;
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/** A stretch of time cut into equal steps; every chart on the tab shares one, so a single cursor lines them up. */
export type TimeWindow = { readonly from: number; readonly to: number; readonly stepMs: number };

const STEPS_MS = [MINUTE_MS, 5 * MINUTE_MS, 15 * MINUTE_MS, 30 * MINUTE_MS, HOUR_MS, 3 * HOUR_MS, 6 * HOUR_MS, 12 * HOUR_MS, DAY_MS];

/** The first round step of at least `ms`, or whole days past the last. */
const roundStep = (ms: number) => STEPS_MS.find((step) => step >= ms) ?? Math.ceil(ms / DAY_MS) * DAY_MS;

/** The finest step each source's points fill: a probe interval, an hour, 6 hours. */
const SOURCE_STEP_MS = { raw: 0, hourly: HOUR_MS, sixHourly: 6 * HOUR_MS } satisfies Record<PointSource, number>;

/**
 * `window` cut into at most 288 round steps, never finer than the points the server reads for it fill, or the line would
 * break between them.
 */
export function chartWindow(window: RangeWindow, now: number, intervalMs: number): TimeWindow {
  const source = pointSource(window, now);
  const floor = source === "raw" ? intervalMs : SOURCE_STEP_MS[source];
  return { ...window, stepMs: roundStep(Math.max(floor, (window.to - window.from) / 288)) };
}

/** An incident's span; an open one runs to `to`. */
export const incidentSpan = (incident: UptimeIncidentEntry, to: number) =>
  ({ from: Date.parse(incident.openedAt), to: incident.closedAt === null ? to : Date.parse(incident.closedAt) });

/** The window a zoomed incident shows: five times its length and at least three hours, centred on it and never past `now`. */
export function incidentRange(incident: UptimeIncidentEntry, now: number): RangeWindow {
  const span = incidentSpan(incident, now);
  const length = Math.max(3 * HOUR_MS, (span.to - span.from) * 5);
  const to = Math.min(now, Math.ceil((span.from + span.to + length) / 2 / MINUTE_MS) * MINUTE_MS);
  return { from: to - length, to };
}

/**
 * The step a time falls in. The first step starts at `from`, so a range's partial first step is not lost, and a time at
 * `to` falls in the last step, since reads include it.
 */
export const stepAt = (window: TimeWindow, at: number) =>
  Math.max(window.from, Math.floor(Math.min(at, window.to - 1) / window.stepMs) * window.stepMs);

/** The start of each step in a window: `from`, then each round step after it. */
export function steps(window: TimeWindow): number[] {
  const first = Math.ceil(window.from / window.stepMs) * window.stepMs;
  const round = Array.from({ length: Math.max(0, Math.ceil((window.to - first) / window.stepMs)) }, (_, index) => first + index * window.stepMs);
  return first > window.from && window.from < window.to ? [window.from, ...round] : round;
}

/** A number for each of `keys`. */
export function tally<K extends string>(keys: readonly K[], value: (key: K) => number): Record<K, number> {
  // SAFETY: the entries hold exactly one value for each of `keys`.
  return Object.fromEntries(keys.map((key) => [key, value(key)])) as Record<K, number>;
}

export const failureTotal = (failures: FailureCounts) => FAILURE_CLASSES.reduce((sum, cls) => sum + failures[cls], 0);

export const passes = (point: UptimePoint) => point.probes - failureTotal(point.failures);

/**
 * Two points of one region as one, keeping `a`'s `at`. Counts and phase sums add, the max is the larger, the median is
 * the medians averaged by passing probes, and the newer error wins. The 90d read in uptime-status.server.ts mirrors it.
 */
export function mergePoints(a: UptimePoint, b: UptimePoint): UptimePoint {
  const weightA = a.medianMs === null ? 0 : passes(a);
  const weightB = b.medianMs === null ? 0 : passes(b);
  const maxes = [a.maxMs, b.maxMs].filter((ms) => ms !== null);
  const newer = a.lastError === null || (b.lastError !== null && Date.parse(b.lastError.at) > Date.parse(a.lastError.at));
  return {
    at: a.at,
    region: a.region,
    probes: a.probes + b.probes,
    failures: tally(FAILURE_CLASSES, (cls) => a.failures[cls] + b.failures[cls]),
    medianMs: weightA + weightB === 0 ? null : ((a.medianMs ?? 0) * weightA + (b.medianMs ?? 0) * weightB) / (weightA + weightB),
    maxMs: maxes.length === 0 ? null : Math.max(...maxes),
    phaseMsSum: tally(PHASES, (phase) => a.phaseMsSum[phase] + b.phaseMsSum[phase]),
    lastError: newer ? b.lastError : a.lastError,
  };
}

/** One step of the response time chart: each region with a counted probe, by name, and their median together. */
export type UptimeStep = { readonly at: number; readonly regions: readonly UptimePoint[]; readonly medianMs: number | null };

/** The window's steps, each region's points in it merged into one. The median is null where none passed, so the line breaks. */
export function checkSteps(points: readonly UptimePoint[], window: TimeWindow): UptimeStep[] {
  const byStep = new Map<number, Map<string, UptimePoint>>();
  for (const point of points) {
    if (point.probes === 0) continue;
    const step = stepAt(window, Date.parse(point.at));
    const regions = byStep.get(step) ?? new Map<string, UptimePoint>();
    const merged = regions.get(point.region);
    regions.set(point.region, merged ? mergePoints(merged, point) : { ...point, at: new Date(step).toISOString() });
    byStep.set(step, regions);
  }
  return steps(window).map((at) => {
    const regions = [...(byStep.get(at)?.values() ?? [])].sort((a, b) => a.region.localeCompare(b.region));
    const all = regions.length === 0 ? null : regions.reduce(mergePoints);
    return { at, regions, medianMs: all?.medianMs ?? null };
  });
}

/**
 * One bar of the history strip: a check result, or a span of time. `tone` is red for a failed result or a span an
 * incident touched, green for a passed result or a checked span, grey otherwise.
 */
export type HistoryBar = {
  readonly start: number;
  /** When the bar's span ends; a result's bar ends where it starts. */
  readonly end: number;
  readonly tone: "up" | "down" | "none";
  readonly result: UptimeCheckResult | null;
  readonly incidents: readonly UptimeIncidentEntry[];
};

/** A span bar is at least 5 minutes and the strip at most 90 of them. */
export const barMs = (window: RangeWindow) => roundStep(Math.max(5 * MINUTE_MS, (window.to - window.from) / 90));

/**
 * The check window as bars, oldest first: one per result when the server sent them, else spans of `barMs`. A span is
 * checked when a point or an incident falls in it.
 */
export function historyBars(window: RangeWindow, uptime: Pick<ServiceUptime, "points" | "checks" | "incidents"> | null): HistoryBar[] {
  const spans = (uptime?.incidents ?? []).map((incident) => ({ incident, ...incidentSpan(incident, window.to) }));
  const touching = (overlaps: (span: { from: number; to: number }) => boolean) =>
    spans.flatMap((span) => overlaps(span) ? [span.incident] : []);
  if (uptime?.checks?.length) {
    return uptime.checks.map((result) => {
      const at = Date.parse(result.at);
      const tone = result.verdict === "pass" ? "up" : result.verdict === "fail" ? "down" : "none";
      return { start: at, end: at, tone, result, incidents: touching((span) => span.from <= at && span.to >= at) };
    });
  }
  const spanned = { ...window, stepMs: barMs(window) };
  const probed = new Set(uptime?.points.map((point) => stepAt(spanned, Date.parse(point.at))));
  const starts = steps(spanned);
  return starts.map((start, index) => {
    const end = starts[index + 1] ?? window.to;
    const touched = touching((span) => span.from < end && span.to > start);
    const tone = touched.length > 0 ? "down" : probed.has(start) ? "up" : "none";
    return { start, end, tone, result: null, incidents: touched };
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
