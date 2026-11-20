import { describe, expect, it } from "vitest";
import type { CrashMarker, HostHistory, HostPoint, MetricRead, ServiceHistory, ServiceOnServer, ServicePoint } from "./metrics";
import { serverMetrics, serviceMetrics } from "./metrics-model";

const [A, B, C, D] = ["a", "b", "c", "d"];
const names = new Map([[A, "web-10"], [B, "web-2"], [C, "db-1"]]);
const window = { from: 0, to: 10_000 };

const onServer = (machine: string, points: readonly ServicePoint[], extra: Partial<ServiceOnServer> = {}): ServiceOnServer =>
  ({ machine, points, gaps: [], markers: [], ...extra });
/** What a Server that runs no readings of the Service reports: no points, and a gap over the range. */
const nothing = (machine: string, extra: Partial<ServiceOnServer> = {}) => onServer(machine, [], { gaps: [{ from: window.from, to: window.to }], ...extra });
const readService = (history: Partial<ServiceHistory>): MetricRead<ServiceHistory> => ({
  type: "history", window, at: window.to, history: { stepMs: 1000, points: [], servers: [], ...history },
});
const readServer = (history: Partial<HostHistory>): MetricRead<HostHistory> => ({
  type: "history", window, at: window.to, history: { stepMs: 1000, points: [], gaps: [], ...history },
});
const marker = (machine: string, at: number, fields: Pick<CrashMarker, "kind"> & { exitCode?: number }): CrashMarker => ({ machine, at, replica: "web-1", ...fields });

describe("serviceMetrics availability", () => {
  const point = [{ at: 0, cpu: 1 }];

  it("is normal when every Server answers with readings", () => {
    expect(serviceMetrics(readService({ points: point, servers: [onServer(A, point)] }), names).availability).toEqual({ type: "normal" });
  });

  it("has no readings when no Server is asked, or every answer is empty", () => {
    expect(serviceMetrics(readService({}), names).availability).toEqual({ type: "no_readings" });
    expect(serviceMetrics(readService({ servers: [nothing(A), nothing(B)] }), names).availability).toEqual({ type: "no_readings" });
    expect(serviceMetrics(readService({ points: [{ at: 0 }], servers: [onServer(A, [{ at: 0 }])] }), names).availability).toEqual({ type: "no_readings" });
  });

  it("has empty series over the window when Cloud keeps no metrics or the Server is silent", () => {
    for (const type of ["not_configured", "not_reporting"] as const) {
      expect(serviceMetrics({ type, window, at: window.to }, names)).toEqual({
        availability: { type },
        window,
        stepMs: null,
        all: { cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [] },
        allGaps: [],
        byServer: [],
        markers: [],
      });
    }
  });
});

describe("serviceMetrics series", () => {
  it("carries each field into its series and skips a field a bucket lacks", () => {
    const points = [
      { at: 1000, cpu: 1.5, memory: 512, memoryLimit: 1024, netIn: 10, netOut: 20, diskRead: 30, diskWrite: 40 },
      { at: 2000, cpu: 0.25 },
    ];
    const metrics = serviceMetrics(readService({ points, servers: [onServer(A, points)] }), names);
    expect(metrics.stepMs).toBe(1000);
    expect(metrics.all).toEqual({
      cpu: [{ at: 1000, value: 1.5 }, { at: 2000, value: 0.25 }],
      memory: [{ at: 1000, value: 512 }],
      memoryLimit: [{ at: 1000, value: 1024 }],
      netIn: [{ at: 1000, value: 10 }],
      netOut: [{ at: 1000, value: 20 }],
      diskRead: [{ at: 1000, value: 30 }],
      diskWrite: [{ at: 1000, value: 40 }],
    });
  });

  const cpu = (at: number, value = 1): ServicePoint => ({ at, cpu: value });
  const every = (from: number, to: number, value?: number) => Array.from({ length: (to - from) / 1000 }, (_, index) => cpu(from + index * 1000, value));

  it("keeps a Service's readings when another Server answers that it has none", () => {
    const onA = every(0, 10_000, 0.5);
    const metrics = serviceMetrics(readService({ points: onA, servers: [onServer(A, onA), nothing(B)] }), names);
    expect(metrics.availability).toEqual({ type: "normal" });
    expect(metrics.allGaps).toEqual([]);
    expect(metrics.all.cpu).toEqual(onA.map((point) => ({ at: point.at, value: 0.5 })));
    expect(metrics.byServer.map((entry) => entry.server.id)).toEqual([A]);
  });

  it("keeps the early readings of one Server when another starts mid-range", () => {
    const top = [...every(0, 5000), ...every(5000, 10_000, 2)];
    const metrics = serviceMetrics(readService({
      points: top,
      servers: [onServer(A, every(0, 10_000)), onServer(B, every(5000, 10_000), { gaps: [{ from: 0, to: 5000 }] })],
    }), names);
    expect(metrics.allGaps).toEqual([]);
    expect(metrics.all.cpu.map((sample) => sample.value)).toEqual([1, 1, 1, 1, 1, 2, 2, 2, 2, 2]);
    expect(metrics.byServer.find((entry) => entry.server.id === B)?.gaps).toEqual([{ from: 0, to: 5000 }]);
  });

  it("gaps the sum only where no Server has data", () => {
    const top = [cpu(0), cpu(1000), cpu(2000), cpu(2600), cpu(3000), cpu(4000)];
    const metrics = serviceMetrics(readService({
      points: top,
      servers: [
        onServer(A, [cpu(0), cpu(3000), cpu(4000)], { gaps: [{ from: 1000, to: 3000 }] }),
        onServer(B, [cpu(0), cpu(1000), cpu(2000)], { gaps: [{ from: 2500, to: 10_000 }] }),
      ],
    }), names);
    expect(metrics.availability).toEqual({ type: "normal" });
    expect(metrics.allGaps).toEqual([{ from: 2500, to: 3000 }]);
    expect(metrics.all.cpu.map((sample) => sample.at)).toEqual([0, 1000, 2000, 3000, 4000]);
    const a = metrics.byServer.find((entry) => entry.server.id === A);
    expect(a?.gaps).toEqual([{ from: 1000, to: 3000 }]);
    expect(a?.series.cpu.map((sample) => sample.at)).toEqual([0, 3000, 4000]);
  });

  it("lists Servers with readings or markers, by name with numbers in order", () => {
    const some = [{ at: 0, memory: 1 }];
    const metrics = serviceMetrics(readService({
      points: some,
      servers: [onServer(A, some), nothing(C), onServer(D, [{ at: 0 }]), nothing(B, { markers: [marker(B, 0, { kind: "oom" })] })],
    }), names);
    expect(metrics.byServer.map((entry) => entry.server.name)).toEqual(["web-2", "web-10"]);
    expect(metrics.byServer[1]?.series.memory).toEqual([{ at: 0, value: 1 }]);
  });

  it("collects markers from every Server in time order with their Server", () => {
    const metrics = serviceMetrics(readService({
      servers: [
        onServer(A, [], { markers: [marker(A, 3000, { kind: "exit", exitCode: 137 })] }),
        onServer(B, [], { markers: [marker(B, 1000, { kind: "oom" }), marker(B, 2000, { kind: "exit" })] }),
      ],
    }), names);
    expect(metrics.markers).toEqual([
      { kind: "oom", at: 1000, server: { id: B, name: "web-2" }, replica: "web-1" },
      { kind: "exit", at: 2000, server: { id: B, name: "web-2" }, replica: "web-1" },
      { kind: "exit", at: 3000, server: { id: A, name: "web-10" }, replica: "web-1", exitCode: 137 },
    ]);
  });
});

describe("serverMetrics", () => {
  it("projects CPU, used memory, visible interfaces and disk, skipping missing fields and gapped samples", () => {
    const points: HostPoint[] = [
      {
        at: 0,
        cpuBusy: 0.25,
        memory: { total: 100, available: 40 },
        interfaces: { lo: { rx: 1, tx: 1 }, eth1: { rx: 5, tx: 6 }, eth0: { rx: 3, tx: 4 }, veth12: { rx: 1, tx: 1 }, docker0: { rx: 1, tx: 1 }, "br-abc": { rx: 1, tx: 1 } },
        disk: { total: 1000, free: 400, volumes: 300, mirrors: 100, logsAndMetrics: 50, other: 150 },
      },
      { at: 1000, cpuBusy: 0.9 },
      { at: 2000, disk: { total: 1000, free: 390 } },
    ];
    const metrics = serverMetrics(readServer({ points, gaps: [{ from: 1000, to: 2000 }] }));
    expect(metrics).toEqual({
      availability: { type: "normal" },
      window,
      stepMs: 1000,
      cpu: [{ at: 0, value: 0.25 }],
      memoryUsed: [{ at: 0, value: 60 }],
      memoryTotal: [{ at: 0, value: 100 }],
      interfaces: [
        { name: "eth0", netIn: [{ at: 0, value: 3 }], netOut: [{ at: 0, value: 4 }] },
        { name: "eth1", netIn: [{ at: 0, value: 5 }], netOut: [{ at: 0, value: 6 }] },
      ],
      disk: [
        { at: 0, total: 1000, free: 400, volumes: 300, mirrors: 100, logsAndMetrics: 50, other: 150 },
        { at: 2000, total: 1000, free: 390 },
      ],
      gaps: [{ from: 1000, to: 2000 }],
    });
  });

  it("has no readings for a Server with nothing in range, and names the off states", () => {
    expect(serverMetrics(readServer({})).availability).toEqual({ type: "no_readings" });
    expect(serverMetrics({ type: "not_reporting", window, at: window.to }).availability).toEqual({ type: "not_reporting" });
    expect(serverMetrics({ type: "not_configured", window, at: window.to }).availability).toEqual({ type: "not_configured" });
  });
});
