import { describe, expect, it } from "vitest";
import {
  causeText, checkSeries, checkState, formatSpan, historyDays, observationText, rangeCovering, uncheckedDays, unreachableHost,
  uptimePercent,
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
  uptime: { "24h": null, "7d": null, "90d": null },
  days: [],
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

describe("historyDays", () => {
  it("marks each day an incident touched, including one still open", () => {
    const first = Date.parse(at);
    const to = first + 3 * DAY;
    const days = [0, 1, 2].map((index) => ({ start: new Date(first + index * DAY).toISOString(), checked: index === 0 }));
    const open = incident([observation({ status: 502 })], new Date(to - DAY - HOUR).toISOString());
    expect(historyDays({ days, incidents: [open] }, to).map((day) => [day.checked, day.incidents.length]))
      .toEqual([[true, 0], [true, 1], [true, 1]]);
  });

  it("draws ninety grey days for a Service without a check", () => {
    const days = historyDays({ days: uncheckedDays(Date.parse(at) + HOUR), incidents: [] }, Date.parse(at));
    expect(days).toHaveLength(90);
    expect(days.every((day) => !day.checked)).toBe(true);
    expect(days.at(-1)?.start).toBe(Date.parse(at));
  });
});

describe("formatSpan", () => {
  it("rounds to minutes, hours and days", () => {
    expect(formatSpan(18 * 60_000)).toBe("18m");
    expect(formatSpan(125 * 60_000)).toBe("2h 5m");
    expect(formatSpan(76 * HOUR)).toBe("3d 4h");
  });
});
