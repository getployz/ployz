import type { HostPoint, MachineId, MetricHistory, MetricHistoryReply, MetricMarker, ServicePoint } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { MetricRead } from "./metrics";
import { containerId, machineId, serviceId } from "#/test/ids";
import { serverMetrics, serviceMetrics } from "./metrics-model";

type RpcError = MetricHistory["unanswered"][number]["error"];

const A = machineId("a");
const B = machineId("b");
const C = machineId("c");
const D = machineId("d");
const names = new Map([[A, "web-10"], [B, "web-2"], [C, "db-1"]]);
const window = { from: 0, to: 10_000 };
const ms = (value: number) => value * 1e6;

const refusal: RpcError = { code: "unsupported", message: "old", details: { capability: "ployz.metrics.history.v1", target: "x" } };
const down: RpcError = { code: "unavailable", message: "the Server is not up, so it was not asked", details: null };

const reply = (points: MetricHistoryReply["points"], extra: Partial<MetricHistoryReply> = {}): MetricHistoryReply =>
  ({ points, step_nanos: ms(1000), gaps: [], ...extra });
// What the daemon sends from a Server without the Service, or for a read that timed out: no points, one gap over the range.
const nothing = (reason: "not_sampled" | "timeout" = "not_sampled"): MetricHistoryReply =>
  reply({ service: [] }, { gaps: [{ from: ms(window.from), to: ms(window.to), reason }] });
const read = (history: Partial<MetricHistory>): MetricRead => ({
  type: "history",
  window,
  at: window.to,
  history: { step_nanos: ms(1000), points: { service: [] }, machines: [], unanswered: [], ...history },
});
const answering = (machine: MachineId, value: MetricHistoryReply) => ({ machine_id: machine, value });
const failing = (machine: MachineId, error: RpcError) => ({ machine_id: machine, error });
const servicePoint = (atMs: number, fields: Omit<ServicePoint, "ts"> = {}): ServicePoint => ({ ts: ms(atMs), ...fields });
type MarkerFields =
  | { readonly kind: "oom" | "restart"; readonly deployment_id?: string }
  | { readonly kind: "exit"; readonly exit_code?: number; readonly deployment_id?: string };
const marker = (machine: MachineId, atMs: number, fields: MarkerFields): MetricMarker => ({
  machine_id: machine,
  container_id: containerId("c"),
  service_id: serviceId("s"),
  service: "ns/web",
  replica: "web-1",
  ts: ms(atMs),
  ...(fields.kind === "exit" ? { oom: false, stopped: false, ...fields } : fields),
});

describe("serviceMetrics availability", () => {
  const point = { service: [servicePoint(0, { cpu_usec: 1 })] };

  it("is normal when every Server answers with readings", () => {
    expect(serviceMetrics(read({ points: point, machines: [answering(A, reply(point))] }), names).availability).toEqual({ type: "normal" });
  });

  it("is mixed when some answer, splitting refusals from silence and naming each Server", () => {
    const metrics = serviceMetrics(read({
      points: point,
      machines: [answering(A, reply(point))],
      unanswered: [failing(B, refusal), failing(D, down)],
    }), names);
    expect(metrics.availability).toEqual({
      type: "mixed",
      needsUpgrade: [{ id: B, name: "web-2" }],
      unanswered: [{ id: D, name: null }],
    });
  });

  it("needs upgrade when none answer and every one refused for the metrics capability", () => {
    expect(serviceMetrics(read({ unanswered: [failing(B, refusal), failing(C, refusal)] }), names).availability).toEqual({
      type: "needs_upgrade",
      servers: [{ id: B, name: "web-2" }, { id: C, name: "db-1" }],
    });
  });

  it("treats an unsupported refusal for another capability as not answering", () => {
    const other: RpcError = { ...refusal, details: { capability: "ployz.logs.tail.v1" } };
    expect(serviceMetrics(read({ unanswered: [failing(B, other)] }), names).availability).toEqual({
      type: "unanswered",
      servers: [{ id: B, name: "web-2" }],
      needsUpgrade: [],
    });
  });

  it("names the Servers that did not answer, not mixed, when the ones that answered have nothing", () => {
    expect(serviceMetrics(read({ machines: [answering(A, nothing())], unanswered: [failing(B, refusal)] }), names).availability).toEqual({
      type: "needs_upgrade",
      servers: [{ id: B, name: "web-2" }],
    });
    expect(serviceMetrics(read({ machines: [answering(A, nothing())], unanswered: [failing(B, refusal), failing(D, down)] }), names).availability).toEqual({
      type: "unanswered",
      servers: [{ id: D, name: null }],
      needsUpgrade: [{ id: B, name: "web-2" }],
    });
  });

  it("is unanswered when none answer and any did not, still listing the ones that need upgrade", () => {
    expect(serviceMetrics(read({ unanswered: [failing(B, refusal), failing(D, down)] }), names).availability).toEqual({
      type: "unanswered",
      servers: [{ id: D, name: null }],
      needsUpgrade: [{ id: B, name: "web-2" }],
    });
  });

  it("has no readings when no Server is asked, or every answer is empty", () => {
    expect(serviceMetrics(read({}), names).availability).toEqual({ type: "no_readings" });
    expect(serviceMetrics(read({ machines: [answering(A, nothing()), answering(B, nothing())] }), names).availability).toEqual({ type: "no_readings" });
    const empty = { service: [servicePoint(0)] };
    expect(serviceMetrics(read({ points: empty, machines: [answering(A, reply(empty))] }), names).availability).toEqual({ type: "no_readings" });
  });

  it("is offline with empty series over the read's window", () => {
    expect(serviceMetrics({ type: "offline", window, at: window.to }, names)).toEqual({
      availability: { type: "offline" },
      window,
      stepMs: null,
      all: { cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [] },
      allGaps: [],
      byServer: [],
      markers: [],
    });
  });
});

describe("serviceMetrics series", () => {
  it("converts nanoseconds to milliseconds and microseconds per second to vCPUs, skipping missing fields", () => {
    const points = { service: [
      servicePoint(1000, { cpu_usec: 1_500_000, mem_bytes: 512, mem_limit: 1024, rx: 10, tx: 20, io_r: 30, io_w: 40 }),
      servicePoint(2000, { cpu_usec: 250_000 }),
    ] };
    const metrics = serviceMetrics(read({ points, machines: [answering(A, reply(points))] }), names);
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

  const cpu = (t: number, usec = 1_000_000) => servicePoint(t, { cpu_usec: usec });
  const every = (from: number, to: number, usec?: number) => Array.from({ length: (to - from) / 1000 }, (_, index) => cpu(from + index * 1000, usec));

  it("keeps a Service's readings when another Server answers that it has none", () => {
    const onA = { service: every(0, 10_000, 500_000) };
    const metrics = serviceMetrics(read({ points: onA, machines: [answering(A, reply(onA)), answering(B, nothing())] }), names);
    expect(metrics.availability).toEqual({ type: "normal" });
    expect(metrics.allGaps).toEqual([]);
    expect(metrics.all.cpu).toEqual(onA.service.map((point) => ({ at: point.ts / 1e6, value: 0.5 })));
    expect(metrics.byServer.map((entry) => entry.server.id)).toEqual([A]);
  });

  it("keeps the early readings of one Server when another starts mid-range", () => {
    const onA = every(0, 10_000);
    const onB = every(5000, 10_000);
    const top = { service: [...every(0, 5000), ...every(5000, 10_000, 2_000_000)] };
    const metrics = serviceMetrics(read({
      points: top,
      machines: [
        answering(A, reply({ service: onA })),
        answering(B, reply({ service: onB }, { gaps: [{ from: ms(0), to: ms(5000), reason: "not_sampled" }] })),
      ],
    }), names);
    expect(metrics.allGaps).toEqual([]);
    expect(metrics.all.cpu.map((sample) => sample.value)).toEqual([1, 1, 1, 1, 1, 2, 2, 2, 2, 2]);
    expect(metrics.byServer.find((entry) => entry.server.id === B)?.gaps).toEqual([{ from: 0, to: 5000, reason: "not_sampled" }]);
  });

  it("gaps the sum where one Server timed out only where no other Server has data", () => {
    const top = { service: [cpu(0), cpu(1000), cpu(2000), cpu(2600), cpu(3000), cpu(4000)] };
    const metrics = serviceMetrics(read({
      points: top,
      machines: [
        answering(A, reply({ service: [cpu(0), cpu(3000), cpu(4000)] }, { gaps: [{ from: ms(1000), to: ms(3000), reason: "not_sampled" }] })),
        answering(B, reply({ service: [cpu(0), cpu(1000), cpu(2000)] }, { gaps: [{ from: ms(2500), to: ms(10_000), reason: "timeout" }] })),
      ],
    }), names);
    expect(metrics.availability).toEqual({ type: "normal" });
    expect(metrics.allGaps).toEqual([{ from: 2500, to: 3000, reason: "timeout" }]);
    expect(metrics.all.cpu.map((sample) => sample.at)).toEqual([0, 1000, 2000, 3000, 4000]);
    const a = metrics.byServer.find((entry) => entry.server.id === A);
    expect(a?.gaps).toEqual([{ from: 1000, to: 3000, reason: "not_sampled" }]);
    expect(a?.series.cpu.map((sample) => sample.at)).toEqual([0, 3000, 4000]);
  });

  it("keeps the sum and names a Server whose read timed out over the whole range as not answering", () => {
    const onA = { service: every(0, 10_000, 500_000) };
    const metrics = serviceMetrics(read({ points: onA, machines: [answering(A, reply(onA)), answering(B, nothing("timeout"))] }), names);
    expect(metrics.availability).toEqual({ type: "mixed", needsUpgrade: [], unanswered: [{ id: B, name: "web-2" }] });
    expect(metrics.allGaps).toEqual([]);
    expect(metrics.all.cpu).toEqual(onA.service.map((point) => ({ at: point.ts / 1e6, value: 0.5 })));
    expect(metrics.byServer.map((entry) => entry.server.id)).toEqual([A]);
  });

  it("names every Server that timed out when none has readings", () => {
    const metrics = serviceMetrics(read({ machines: [answering(A, nothing("timeout")), answering(B, nothing())] }), names);
    expect(metrics.availability).toEqual({ type: "unanswered", servers: [{ id: A, name: "web-10" }], needsUpgrade: [] });
    expect(metrics.allGaps).toEqual([{ from: 0, to: 10_000, reason: "not_sampled" }]);
    expect(metrics.byServer).toEqual([]);
  });

  it("lists answering Servers with readings or markers, by name with numbers in order", () => {
    const some = { service: [servicePoint(0, { mem_bytes: 1 })] };
    const metrics = serviceMetrics(read({
      points: some,
      machines: [
        answering(A, reply(some)),
        answering(C, nothing()),
        answering(D, reply({ service: [servicePoint(0)] })),
        answering(B, { ...nothing(), markers: [marker(B, 0, { kind: "restart" })] }),
      ],
    }), names);
    expect(metrics.byServer.map((entry) => entry.server.name)).toEqual(["web-2", "web-10"]);
    expect(metrics.byServer[1]?.series.memory).toEqual([{ at: 0, value: 1 }]);
  });

  it("collects markers from every reply in time order with their Server", () => {
    const metrics = serviceMetrics(read({
      machines: [
        answering(A, reply({ service: [] }, { markers: [marker(A, 3000, { kind: "exit", exit_code: 137, deployment_id: "dep-1" })] })),
        answering(B, reply({ service: [] }, { markers: [marker(B, 1000, { kind: "oom" }), marker(B, 2000, { kind: "exit" })] })),
      ],
    }), names);
    expect(metrics.markers).toEqual([
      { kind: "oom", at: 1000, server: { id: B, name: "web-2" }, replica: "web-1" },
      { kind: "exit", at: 2000, server: { id: B, name: "web-2" }, replica: "web-1" },
      { kind: "exit", at: 3000, server: { id: A, name: "web-10" }, replica: "web-1", exitCode: 137, deploymentId: "dep-1" },
    ]);
  });
});

describe("serverMetrics", () => {
  const host = (atMs: number, fields: Omit<HostPoint, "ts">): HostPoint => ({ ts: ms(atMs), ...fields });

  it("projects CPU, used memory, visible interfaces and disk, skipping missing fields and gapped samples", () => {
    const points = { machine: [
      host(0, {
        cpu_busy: 0.25,
        memory: { total: 100, available: 40 },
        interfaces: { lo: { rx: 1, tx: 1 }, eth1: { rx: 5, tx: 6 }, eth0: { rx: 3, tx: 4 }, veth12: { rx: 1, tx: 1 }, docker0: { rx: 1, tx: 1 }, "br-abc": { rx: 1, tx: 1 } },
        disk: { total: 1000, free: 400, volumes: 300, mirror_slots: 100, observe_store: 50, other: 150 },
      }),
      host(1000, { cpu_busy: 0.9 }),
      host(2000, { disk: { total: 1000, free: 390 } }),
    ] };
    const metrics = serverMetrics(read({
      points,
      machines: [answering(A, reply(points, { gaps: [{ from: ms(1000), to: ms(2000), reason: "timeout" }] }))],
    }), names);
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
      gaps: [{ from: 1000, to: 2000, reason: "timeout" }],
    });
  });

  it("merges overlapping gaps as a timeout when either one is", () => {
    const metrics = serverMetrics(read({
      points: { machine: [] },
      machines: [answering(A, reply({ machine: [] }, { gaps: [
        { from: ms(0), to: ms(1500), reason: "not_sampled" },
        { from: ms(1000), to: ms(2000), reason: "timeout" },
        { from: ms(5000), to: ms(6000), reason: "not_sampled" },
      ] }))],
    }), names);
    expect(metrics.gaps).toEqual([{ from: 0, to: 2000, reason: "timeout" }, { from: 5000, to: 6000, reason: "not_sampled" }]);
  });

  it("classifies a Server that refused or did not answer", () => {
    const empty = { points: { machine: [] } };
    expect(serverMetrics(read({ ...empty, unanswered: [failing(B, refusal)] }), names).availability)
      .toEqual({ type: "needs_upgrade", servers: [{ id: B, name: "web-2" }] });
    expect(serverMetrics(read({ ...empty, unanswered: [failing(C, down)] }), names).availability)
      .toEqual({ type: "unanswered", servers: [{ id: C, name: "db-1" }], needsUpgrade: [] });
    expect(serverMetrics(read({ ...empty, machines: [answering(A, reply({ machine: [] }))] }), names).availability)
      .toEqual({ type: "no_readings" });
    const timedOut = reply({ machine: [] }, { gaps: [{ from: ms(window.from), to: ms(window.to), reason: "timeout" }] });
    expect(serverMetrics(read({ ...empty, machines: [answering(A, timedOut)] }), names))
      .toMatchObject({ availability: { type: "unanswered", servers: [{ id: A, name: "web-10" }], needsUpgrade: [] }, gaps: [] });
  });
});
