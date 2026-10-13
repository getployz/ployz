import { describe, expect, it } from "vitest";
import type { ServiceMetrics, ServiceSeries } from "#/modules/metrics/metrics-model";
import { diskSegments, serviceUsageOn } from "./server-usage";

const window = { from: 0, to: 120_000 };
const at = (...values: number[]) => values.map((value, index) => ({ at: index * 60_000, value }));

const series = (extra: Partial<ServiceSeries>): ServiceSeries => ({
  cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [], ...extra,
});

describe("diskSegments", () => {
  it("splits used bytes by what holds them, then free", () => {
    expect(diskSegments({ at: 0, total: 1800, free: 1390, volumes: 214, mirrors: 96, logsAndMetrics: 38, other: 62 }).map((segment) => [segment.label, segment.bytes]))
      .toEqual([["Volumes", 214], ["Mirrors", 96], ["Logs and metrics", 38], ["Other", 62], ["Free", 1390]]);
  });

  it("shows used bytes as one when the Server reports no breakdown", () => {
    expect(diskSegments({ at: 0, total: 1800, free: 1390 }).map((segment) => [segment.label, segment.bytes]))
      .toEqual([["Used", 410], ["Free", 1390]]);
  });

  it("never calls the bytes an unmeasured pool may hold Other", () => {
    expect(diskSegments({ at: 0, total: 1800, free: 1390, logsAndMetrics: 38 }).map((segment) => [segment.label, segment.bytes]))
      .toEqual([["Logs and metrics", 38], ["Used", 372], ["Free", 1390]]);
  });

  it("leaves the rest Used when logs and metrics went unmeasured", () => {
    expect(diskSegments({ at: 0, total: 1800, free: 1390, volumes: 214, mirrors: 96 }).map((segment) => [segment.label, segment.bytes]))
      .toEqual([["Volumes", 214], ["Mirrors", 96], ["Used", 100], ["Free", 1390]]);
  });
});

describe("serviceUsageOn", () => {
  const metrics = (byServer: ServiceMetrics["byServer"]): ServiceMetrics => ({
    availability: { type: "normal" }, window, stepMs: 60_000, all: series({}), allGaps: [], byServer, markers: [],
  });
  const fra1 = { id: "a".repeat(32), name: "fra-1" };
  const fra2 = { id: "b".repeat(32), name: "fra-2" };

  it("reads this Server's share, not the sum", () => {
    const usage = serviceUsageOn(metrics([
      { server: fra1, series: series({ cpu: at(0.1, 0.24), memory: at(300, 318) }), gaps: [] },
      { server: fra2, series: series({ cpu: at(2), memory: at(900) }), gaps: [] },
    ]), fra1.id, 0);
    expect(usage).toEqual({ cpu: 0.24, memory: 318 });
  });

  it("is null when this Server's readings stopped before the read's end", () => {
    const stale = { ...metrics([{ server: fra1, series: series({ cpu: at(0.1, 0.24), memory: at(300, 318) }), gaps: [] }]), window: { from: 0, to: 600_000 } };
    expect(serviceUsageOn(stale, fra1.id, 0)).toBeNull();
  });

  it("is null when this Server sent no readings", () => {
    expect(serviceUsageOn(metrics([{ server: fra2, series: series({ cpu: at(2), memory: at(900) }), gaps: [] }]), fra1.id, 0)).toBeNull();
  });
});
