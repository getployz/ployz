import { describe, expect, it } from "vitest";
import {
  causeText, chartWindow, checkState, checkSteps, failureClass, formatSpan, historyBars, incidentRange, mergePoints,
  observationText, regionLabel, steps, tally, unreachableHost, uptimePercent, wrongPathIncident,
} from "./uptime";
import { pointSource } from "./uptime-range";
import {
  FAILURE_CLASSES, type FailureClass, type IncidentObservation, PHASES, type ServiceUptime, type UptimeCheckResult,
  type UptimeIncidentEntry, type UptimePoint,
} from "./uptime-status";

const at = "2026-10-11T00:00:00.000Z";
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const observation = (patch: Partial<IncidentObservation>): IncidentObservation => ({
  region: "ams", kind: "routine", verdict: "fail", status: 0, failedPhase: null, error: null, remoteIp: null, at, ...patch,
});
const incident = (observations: IncidentObservation[], openedAt = at, closedAt: string | null = null): UptimeIncidentEntry =>
  ({ id: "inc", openedAt, closedAt, observations });
/** One probe: passing in `ms` split evenly over the phases, or failing in `failed`. */
const probe = (region: string, at: number, ms: number | null, failed: FailureClass | null = null): UptimePoint => ({
  at: new Date(at).toISOString(),
  region,
  probes: 1,
  failures: tally(FAILURE_CLASSES, (cls) => cls === failed ? 1 : 0),
  medianMs: ms,
  maxMs: ms,
  phaseMsSum: tally(PHASES, () => (ms ?? 0) / 4),
  lastError: failed === null ? null : { at: new Date(at).toISOString(), cls: failed, text: failed },
});
const uptime = (patch: Partial<ServiceUptime>): ServiceUptime => ({
  check: { id: "chk", url: "https://acme.com/health", intervalSeconds: 300 },
  status: "up",
  uptime: null,
  points: [],
  checks: null,
  incidents: [],
  regions: {},
  ...patch,
});

describe("regionLabel", () => {
  it("names a region, falls back to its code, and labels nothing when one region is in view", () => {
    const regions = { ams: "Amsterdam", sin: null };
    expect(regionLabel(regions, "ams")).toEqual({ text: "Amsterdam", named: true });
    expect(regionLabel(regions, "sin")).toEqual({ text: "sin", named: false });
    expect(regionLabel({ ams: "Amsterdam" }, "ams")).toBeNull();
  });
});

describe("observationText", () => {
  it("names the status, else the error, else the failed phase", () => {
    expect(observationText(observation({ status: 502 }))).toBe("502");
    expect(observationText(observation({ error: "timed_out", failedPhase: "request" }))).toBe("Timeout");
    expect(observationText(observation({ error: "connection_refused", failedPhase: "connect" }))).toBe("Refused");
    expect(observationText(observation({ error: "other", failedPhase: "dns" }))).toBe("DNS");
    expect(observationText(observation({ failedPhase: "tls" }))).toBe("TLS");
    expect(observationText(observation({ error: "other", failedPhase: "request" }))).toBe("Error");
  });
});

describe("failureClass", () => {
  it("puts every failing probe in exactly one class, the HTTP status first", () => {
    const seen = new Set<FailureClass>();
    for (const status of [0, 503]) {
      for (const error of [null, "connection_refused", "timed_out", "other"] as const) {
        for (const failedPhase of [null, "dns", "connect", "tls", "request"] as const) {
          const cls = failureClass({ status, error, failedPhase });
          if (status !== 0) expect(cls).toBe("status");
          seen.add(cls);
        }
      }
    }
    expect([...seen].sort()).toEqual([...FAILURE_CLASSES].sort());
    expect(failureClass({ status: 503, error: null, failedPhase: null })).toBe("status");
  });
});

describe("causeText", () => {
  it("is the first failing observation's answer", () => {
    expect(causeText(incident([observation({ verdict: "pass", status: 200 }), observation({ status: 503 })]))).toBe("503");
    expect(causeText(incident([]))).toBe("Unknown");
  });
});

describe("checkState", () => {
  it("is no_domain without a check, else the server's status", () => {
    expect(checkState(null)).toBe("no_domain");
    expect(checkState(uptime({ status: "pending" }))).toBe("pending");
    expect(checkState(uptime({ status: "unreachable" }))).toBe("unreachable");
  });
});

describe("unreachableHost", () => {
  const check = { serviceId: "web", url: "https://acme.com/health", openIncidentAt: at };
  it("is the check's hostname while an incident is open", () => {
    expect(unreachableHost([check], "web")).toBe("acme.com");
    expect(unreachableHost([{ ...check, openIncidentAt: null }], "web")).toBeNull();
    expect(unreachableHost([check], "api")).toBeNull();
  });
});

describe("wrongPathIncident", () => {
  const open = incident([observation({ verdict: "pass", status: 200 }), observation({ status: 404 })]);
  it("is the open incident of a check that never passed and first failed with a 4xx", () => {
    expect(wrongPathIncident(uptime({ status: "unreachable", incidents: [open] }))).toBe(open);
    expect(wrongPathIncident(uptime({ status: "unreachable", uptime: 0, incidents: [open] }))).toBe(open);
  });

  it("is null once the check has passed in its range, or when the failure is not a 4xx", () => {
    expect(wrongPathIncident(uptime({ status: "unreachable", uptime: 0.5, incidents: [open] }))).toBeNull();
    const closed = incident([observation({ status: 503 })], "2026-10-01T00:00:00.000Z", "2026-10-01T01:00:00.000Z");
    expect(wrongPathIncident(uptime({ status: "unreachable", incidents: [open, closed] }))).toBeNull();
    expect(wrongPathIncident(uptime({ status: "unreachable", incidents: [incident([observation({ status: 502 })])] }))).toBeNull();
    expect(wrongPathIncident(uptime({ status: "unreachable", incidents: [incident([observation({ error: "timed_out" })])] }))).toBeNull();
    expect(wrongPathIncident(uptime({ incidents: [] }))).toBeNull();
  });
});

describe("uptimePercent", () => {
  it("floors to two decimals, so a failure never reads as 100%", () => {
    expect(uptimePercent(0.999_99)).toBe("99.99%");
    expect(uptimePercent(1)).toBe("100%");
    expect(uptimePercent(null)).toBeNull();
  });
});

describe("incidentRange", () => {
  const to = Date.parse(at);
  const closed = (opened: number, minutes: number) =>
    incident([observation({ status: 502 })], new Date(opened).toISOString(), new Date(opened + minutes * 60_000).toISOString());

  it("centres a short incident in three hours", () => {
    const opened = to - 12 * DAY;
    expect(incidentRange(closed(opened, 20), to)).toEqual({ from: opened - 80 * 60_000, to: opened + 100 * 60_000 });
  });

  it("shows five times a long incident and never runs past now", () => {
    expect(incidentRange(incident([observation({ status: 502 })], new Date(to - 30 * DAY).toISOString()), to))
      .toEqual({ from: to - 150 * DAY, to });
  });

  it("draws an unbroken line from the points the server reads for it, however old the incident", () => {
    for (const age of [2 * HOUR, 3 * DAY, 12 * DAY, 80 * DAY]) {
      const range = incidentRange(closed(to - age, 20), to);
      const stepMs = { raw: 5 * 60_000, hourly: HOUR, sixHourly: 6 * HOUR }[pointSource(range, to)];
      const window = chartWindow(range, to, 5 * 60_000);
      const series = steps({ ...range, stepMs }).map((step) => probe("ams", step, 100));
      const line = checkSteps(series, window).map((step) => step.medianMs);
      expect(line.length).toBeGreaterThanOrEqual(3);
      expect(line).not.toContain(null);
    }
  });
});

describe("chartWindow", () => {
  it("cuts a range into at most 288 round steps, never finer than its points", () => {
    const to = Date.parse(at);
    expect(chartWindow({ from: to - 15 * 60_000, to }, to, 60_000).stepMs).toBe(60_000);
    expect(chartWindow({ from: to - 15 * 60_000, to }, to, 300_000).stepMs).toBe(300_000);
    expect(chartWindow({ from: to - DAY, to }, to, 60_000).stepMs).toBe(5 * 60_000);
    expect(chartWindow({ from: to - 7 * DAY, to }, to, 60_000).stepMs).toBe(HOUR);
    expect(chartWindow({ from: to - 30 * DAY, to }, to, 60_000).stepMs).toBe(6 * HOUR);
    expect(steps(chartWindow({ from: 0, to }, to, 60_000)).length).toBeLessThanOrEqual(288);
  });
});

describe("mergePoints", () => {
  it("adds counts and phase sums, keeps the larger max and the newer error, and averages medians by passing probes", () => {
    const hour = { ...probe("ams", 0, 100), probes: 4, medianMs: 100, maxMs: 300, phaseMsSum: tally(PHASES, () => 10),
      failures: tally(FAILURE_CLASSES, (cls) => cls === "tls" ? 1 : 0),
      lastError: { at: new Date(60_000).toISOString(), cls: "tls" as const, text: "TLS · expired" } };
    const later = { ...probe("ams", HOUR, 200), probes: 2, maxMs: 250,
      failures: tally(FAILURE_CLASSES, (cls) => cls === "status" ? 1 : 0),
      lastError: { at: new Date(HOUR + 60_000).toISOString(), cls: "status" as const, text: "503" } };
    expect(mergePoints(hour, later)).toEqual({
      at: hour.at,
      region: "ams",
      probes: 6,
      failures: tally(FAILURE_CLASSES, (cls) => cls === "tls" || cls === "status" ? 1 : 0),
      medianMs: (100 * 3 + 200 * 1) / 4,
      maxMs: 300,
      phaseMsSum: tally(PHASES, () => 60),
      lastError: later.lastError,
    });
    expect(mergePoints(later, hour).lastError).toEqual(later.lastError);
  });

  it("has no median or max when no probe passed", () => {
    const merged = mergePoints(probe("ams", 0, null, "timeout"), probe("ams", 1, null, "dns"));
    expect(merged).toMatchObject({ probes: 2, medianMs: null, maxMs: null, lastError: { cls: "dns" } });
    expect(mergePoints(merged, probe("ams", 2, 80)).medianMs).toBe(80);
  });
});

describe("checkSteps", () => {
  it("merges each region's probes per step and breaks the line where none passed", () => {
    const minute = 60_000;
    const window = { from: 0, to: 15 * minute, stepMs: 5 * minute };
    const points = [
      probe("sjc", minute, 300),
      probe("ams", 0, 100),
      probe("ams", 2 * minute, 200),
      probe("ams", 5 * minute, null, "tls"),
      { ...probe("ams", 10 * minute, null), probes: 0 },
    ];
    const [first, second, third] = checkSteps(points, window);
    expect(first?.regions.map((point) => [point.region, point.at, point.probes, point.medianMs, point.maxMs])).toEqual([
      ["ams", new Date(0).toISOString(), 2, 150, 200],
      ["sjc", new Date(0).toISOString(), 1, 300, 300],
    ]);
    expect(first?.medianMs).toBe(200);
    expect(second).toMatchObject({ medianMs: null, regions: [{ region: "ams", probes: 1, failures: { tls: 1 } }] });
    expect(third).toEqual({ at: 10 * minute, regions: [], medianMs: null });
  });

  it("counts a point before the first step in that step", () => {
    const minute = 60_000;
    const window = { from: 2 * minute, to: 7 * minute, stepMs: 5 * minute };
    expect(checkSteps([probe("ams", 3 * minute, 100)], window).map((step) => step.medianMs)).toEqual([100, null]);
    expect(steps(window)).toEqual([2 * minute, 5 * minute]);
  });

  it("counts a point exactly at Until in the last step", () => {
    const minute = 60_000;
    const window = { from: 0, to: 10 * minute, stepMs: 5 * minute };
    expect(checkSteps([probe("ams", 10 * minute, 100)], window).map((step) => step.medianMs)).toEqual([null, 100]);
  });

  it("draws one step for a range shorter than a step between round steps", () => {
    const minute = 60_000;
    const window = { from: minute, to: 4 * minute, stepMs: 5 * minute };
    expect(checkSteps([probe("ams", 2 * minute, 100)], window).map((step) => step.medianMs)).toEqual([100]);
  });
});

describe("historyBars", () => {
  const to = Date.parse(at);
  const result = (minutesAgo: number, patch: Partial<UptimeCheckResult> = {}): UptimeCheckResult => ({
    at: new Date(to - minutesAgo * 60_000).toISOString(), region: "ams", kind: "routine", verdict: "pass", status: 200, ms: 120, ...patch,
  });

  it("draws one bar per result of any kind, toned by its verdict", () => {
    const open = incident([observation({ status: 502 })], new Date(to - 10 * 60_000).toISOString());
    const checks = [result(20), result(10, { verdict: "fail", status: 502 }), result(9, { kind: "confirm", verdict: "fail", status: 0 }), result(5, { verdict: "inconclusive" })];
    const bars = historyBars({ from: to - HOUR, to }, uptime({ checks, incidents: [open] }));
    expect(bars.map((bar) => [bar.start, bar.tone, bar.incidents.length, bar.result?.kind])).toEqual([
      [to - 20 * 60_000, "up", 0, "routine"],
      [to - 10 * 60_000, "down", 1, "routine"],
      [to - 9 * 60_000, "down", 1, "confirm"],
      [to - 5 * 60_000, "none", 1, "routine"],
    ]);
  });

  it("cuts a window without results into spans, red where an incident touched, green where a point fell", () => {
    const open = incident([observation({ status: 502 })], new Date(to - 2 * HOUR - 1).toISOString());
    const points = [probe("ams", to - 3 * HOUR + 1, null, "dns")];
    const bars = historyBars({ from: to - DAY, to }, uptime({ points, checks: [], incidents: [open] }));
    expect(bars).toHaveLength(48);
    expect(bars.slice(-7).map((bar) => bar.tone)).toEqual(["none", "up", "down", "down", "down", "down", "down"]);
    expect(bars.at(-6)?.start).toBe(to - 3 * HOUR);
  });

  it("draws grey spans for a Service without a check", () => {
    const bars = historyBars({ from: to - 30 * 60_000, to }, null);
    expect(bars).toHaveLength(6);
    expect(bars.every((bar) => bar.tone === "none" && bar.result === null)).toBe(true);
  });
});

describe("formatSpan", () => {
  it("rounds to minutes, hours and days", () => {
    expect(formatSpan(18 * 60_000)).toBe("18m");
    expect(formatSpan(125 * 60_000)).toBe("2h 5m");
    expect(formatSpan(76 * HOUR)).toBe("3d 4h");
  });
});
