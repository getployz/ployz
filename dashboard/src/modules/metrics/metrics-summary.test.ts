import { describe, expect, it } from "vitest";
import type { ServerMetrics, ServiceMetrics, ServiceSeries } from "./metrics-model";
import { bytesText, gapNotes, serverRows, serviceRows } from "./metrics-summary";

const EMPTY: ServiceSeries = { cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [] };
const STEP = 10_000;
const UNTIL = 1_000_000;

const service = (all: Partial<ServiceSeries>): ServiceMetrics => ({
  availability: { type: "normal" }, window: { from: 0, to: UNTIL }, stepMs: STEP,
  all: { ...EMPTY, ...all }, allGaps: [], byServer: [], markers: [],
});

describe("serviceRows", () => {
  it("shows now, avg and peak, with memory against its limit", () => {
    const rows = serviceRows(service({
      cpu: [{ at: UNTIL - 20_000, value: 0.5 }, { at: UNTIL - 10_000, value: 1.5 }],
      memory: [{ at: UNTIL - 10_000, value: 512_000_000 }],
      memoryLimit: [{ at: UNTIL - 10_000, value: 1_000_000_000 }],
    }), UNTIL);
    expect(rows.find((row) => row.metric === "CPU")).toEqual({ metric: "CPU", now: "1.50 vCPU", avg: "1.00 vCPU", peak: "1.50 vCPU" });
    expect(rows.find((row) => row.metric === "Memory")?.now).toBe("512 MB / 1.0 GB");
  });

  it("leaves a metric with no reading empty rather than zero", () => {
    const rows = serviceRows(service({ cpu: [{ at: UNTIL - 10_000, value: 1 }] }), UNTIL);
    expect(rows.find((row) => row.metric === "Net in")).toEqual({ metric: "Net in", now: null, avg: null, peak: null });
  });

  it("has no now once the newest reading is older than three steps", () => {
    const [cpu] = serviceRows(service({ cpu: [{ at: UNTIL - 5 * STEP, value: 2 }] }), UNTIL);
    expect(cpu).toEqual({ metric: "CPU", now: null, avg: "2.00 vCPU", peak: "2.00 vCPU" });
  });

  it("is empty when no Server reported anything", () => {
    expect(serviceRows(service({}), UNTIL)).toEqual([]);
  });
});

describe("serverRows", () => {
  const host: ServerMetrics = {
    availability: { type: "normal" }, window: { from: 0, to: UNTIL }, stepMs: STEP,
    cpu: [{ at: UNTIL - STEP, value: 0.25 }], memoryUsed: [], memoryTotal: [],
    interfaces: [{ name: "eth0", netIn: [{ at: UNTIL - STEP, value: 2_500_000 }], netOut: [] }],
    disk: [{ at: UNTIL - STEP, total: 1000e9, free: 400e9, volumes: 300e9 }], gaps: [],
  };

  it("names each interface and splits the disk by kind", () => {
    const rows = serverRows(host, UNTIL);
    expect(rows.find((row) => row.metric === "CPU")?.now).toBe("25%");
    expect(rows.find((row) => row.metric === "Net eth0 in")?.now).toBe("2.5 MB/s");
    expect(rows.find((row) => row.metric === "Disk")?.now).toBe("600 GB / 1.0 TB");
    expect(rows.find((row) => row.metric === "Volumes")?.now).toBe("300 GB");
    expect(rows.find((row) => row.metric === "Mirrors")).toEqual({ metric: "Mirrors", now: null, avg: null, peak: null });
  });
});

describe("gapNotes", () => {
  it("adds up the spans nothing covers", () => {
    expect(gapNotes([{ from: 0, to: 600_000 }, { from: 700_000, to: 1_000_000 }]))
      .toEqual(["No readings for 15m of the range."]);
    expect(gapNotes([])).toEqual([]);
  });
});

describe("bytesText", () => {
  it("uses decimal units, one decimal under ten", () => {
    expect([bytesText(999), bytesText(1500), bytesText(12_000_000), bytesText(3.2e12)]).toEqual(["999 B", "1.5 KB", "12 MB", "3.2 TB"]);
  });
});
