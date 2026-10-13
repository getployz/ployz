import { describe, expect, it } from "vitest";
import type { ServerMetrics, ServiceMetrics, ServiceSeries } from "#/modules/metrics/metrics-model";
import { diskSegments, serverUsage, serviceUsageOn } from "./server-usage";

const window = { from: 0, to: 120_000 };
const at = (...values: number[]) => values.map((value, index) => ({ at: index * 60_000, value }));

const server = (extra: Partial<ServerMetrics>): ServerMetrics => ({
  availability: { type: "normal" }, window, stepMs: 60_000,
  cpu: [], memoryUsed: [], memoryTotal: [], interfaces: [], disk: [], gaps: [],
  ...extra,
});

const series = (extra: Partial<ServiceSeries>): ServiceSeries => ({
  cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [], ...extra,
});

describe("serverUsage", () => {
  it("reads the latest CPU, memory and disk", () => {
    expect(serverUsage(server({
      cpu: at(0.2, 0.37),
      memoryUsed: at(10, 11),
      memoryTotal: at(32, 32),
      disk: [{ at: 0, total: 1800, free: 1500 }, { at: 60_000, total: 1800, free: 1390 }],
    }))).toEqual({ cpu: 0.37, memory: { used: 11, total: 32 }, disk: { used: 410, total: 1800 } });
  });

  it("has nothing where the Server sent nothing, never a zero", () => {
    expect(serverUsage(server({}))).toEqual({ cpu: null, memory: null, disk: null });
  });
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
    ]), fra1.id);
    expect(usage).toEqual({ cpu: 0.24, memory: 318 });
  });

  it("is null when this Server sent no readings", () => {
    expect(serviceUsageOn(metrics([{ server: fra2, series: series({ cpu: at(2), memory: at(900) }), gaps: [] }]), fra1.id)).toBeNull();
  });
});
