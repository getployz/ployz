import { describe, expect, it } from "vitest";
import {
  barCount, causeText, checkState, checkSteps, failureClass, formatSpan, historyBars, incidentWindow, mergePoints, observationText,
  rangeCovering, steps, tally, unreachableHost, uptimePercent, wrongPathIncident,
} from "./uptime";
import { FAILURE_CLASSES, type FailureClass, type IncidentObservation, PHASES, type ServiceUptime, type UptimeIncidentEntry, type UptimePoint } from "./uptime-status";

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
  incidents: [],
  ...patch,
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

describe("rangeCovering", () => {
  it("picks the shortest range that reaches the zoom's start", () => {
    const to = Date.parse(at);
    expect(rangeCovering(to - 3 * HOUR, to)).toBe("24h");
    expect(rangeCovering(to - 2 * DAY, to)).toBe("7d");
    expect(rangeCovering(to - 30 * DAY, to)).toBe("90d");
    expect(rangeCovering(to - 200 * DAY, to)).toBe("90d");
  });
});

describe("incidentWindow", () => {
  it("draws an unbroken line from the series its zoom reads, however old the incident", () => {
    const to = Date.parse(at);
    for (const age of [2 * HOUR, 3 * DAY, 12 * DAY, 80 * DAY]) {
      const opened = to - age;
      const window = incidentWindow(incident([observation({ status: 502 })], new Date(opened).toISOString(), new Date(opened + 20 * 60_000).toISOString()), to);
      const stepMs = { "24h": 5 * 60_000, "7d": HOUR, "90d": 6 * HOUR }[rangeCovering(window.from, to)];
      const series = steps({ ...window, stepMs }).map((step) => probe("ams", step, 100));
      const line = checkSteps(series, window).map((step) => step.medianMs);
      expect(line.length).toBeGreaterThanOrEqual(12);
      expect(line).not.toContain(null);
      expect(window.from).toBeLessThanOrEqual(opened);
    }
  });

  it("keeps a 20-minute incident 12 days old centred on it", () => {
    const to = Date.parse(at);
    const opened = to - 12 * DAY;
    const window = incidentWindow(incident([observation({ status: 502 })], new Date(opened).toISOString(), new Date(opened + 20 * 60_000).toISOString()), to);
    expect(window).toEqual({ from: opened - 3 * DAY + 6 * HOUR, to: opened + 3 * DAY + 6 * HOUR, stepMs: 6 * HOUR });
  });

  it("stays inside the 90 days for an incident in their oldest days", () => {
    const to = Date.parse(at);
    const opened = to - 89 * DAY;
    const window = incidentWindow(incident([observation({ status: 502 })], new Date(opened).toISOString(), new Date(opened + 20 * 60_000).toISOString()), to);
    expect(window.from).toBe(to - 90 * DAY);
    expect(window.to).toBeLessThanOrEqual(to);
    expect(window.from).toBeLessThanOrEqual(opened);
    expect(window.to).toBeGreaterThanOrEqual(opened + 20 * 60_000);
  });

  it("shows all 90 days for an open incident 30 days long", () => {
    const to = Date.parse(at);
    const window = incidentWindow(incident([observation({ status: 502 })], new Date(to - 30 * DAY).toISOString()), to);
    expect(window).toEqual({ from: to - 90 * DAY, to, stepMs: 18 * HOUR });
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
});

describe("historyBars", () => {
  const to = Date.parse(at);

  it("marks each bar an incident touched, including one still open, as checked", () => {
    const open = incident([observation({ status: 502 })], new Date(to - 2 * HOUR - 1).toISOString());
    const bars = historyBars("24h", to, { points: [], incidents: [open] });
    expect(bars.map((bar) => [bar.checked, bar.incidents.length]).slice(-4)).toEqual([[false, 0], [true, 1], [true, 1], [true, 1]]);
  });

  it("marks the bar of each point as checked", () => {
    const points = [probe("ams", to - 3 * HOUR + 1, null, "dns")];
    const bars = historyBars("7d", to, { points, incidents: [] });
    expect(bars.flatMap((bar, index) => bar.checked ? [index] : [])).toEqual([41]);
    expect(bars[41]?.start).toBe(to - 4 * HOUR);
  });

  it("draws a grey bar per step of the range for a Service without a check", () => {
    for (const [range, count] of [["24h", 24], ["7d", 42], ["90d", 90]] as const) {
      const bars = historyBars(range, to + HOUR / 2, null);
      expect(barCount(range)).toBe(count);
      expect(bars).toHaveLength(count);
      expect(bars.every((bar) => !bar.checked)).toBe(true);
    }
  });
});

describe("formatSpan", () => {
  it("rounds to minutes, hours and days", () => {
    expect(formatSpan(18 * 60_000)).toBe("18m");
    expect(formatSpan(125 * 60_000)).toBe("2h 5m");
    expect(formatSpan(76 * HOUR)).toBe("3d 4h");
  });
});
