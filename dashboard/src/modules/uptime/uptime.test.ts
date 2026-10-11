import { describe, expect, it } from "vitest";
import {
  barCount, causeText, checkSeries, checkState, formatSpan, historyBars, incidentWindow, observationText, rangeCovering, steps, unreachableHost,
  uptimePercent, wrongPathIncident,
} from "./uptime";
import type { IncidentObservation, ServiceUptime, UptimeIncidentEntry } from "./uptime-status";

const at = "2026-10-11T00:00:00.000Z";
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const observation = (patch: Partial<IncidentObservation>): IncidentObservation => ({
  region: "ams", kind: "routine", verdict: "fail", status: 0, failedPhase: null, error: null, remoteIp: null, at, ...patch,
});
const incident = (observations: IncidentObservation[], openedAt = at, closedAt: string | null = null): UptimeIncidentEntry =>
  ({ id: "inc", openedAt, closedAt, observations });
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
      const series = steps({ ...window, stepMs }).map((step) => ({ at: new Date(step).toISOString(), region: "ams", ms: 100 }));
      const line = checkSeries(series, window);
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

describe("checkSeries", () => {
  it("averages every region's passing points per step and breaks where none passed", () => {
    const minute = 60_000;
    const window = { from: 0, to: 15 * minute, stepMs: 5 * minute };
    const points = [
      { at: new Date(0).toISOString(), region: "ams", ms: 100 },
      { at: new Date(minute).toISOString(), region: "sjc", ms: 200 },
      { at: new Date(5 * minute).toISOString(), region: "ams", ms: null },
    ];
    expect(checkSeries(points, window)).toEqual([150, null, null]);
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
    const points = [{ at: new Date(to - 3 * HOUR + 1).toISOString(), region: "ams", ms: null }];
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
