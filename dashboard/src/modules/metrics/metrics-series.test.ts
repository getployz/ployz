import { describe, expect, it } from "vitest";
import { hostPoints, servicePoints, type DiskRow, type MetricRow } from "./metrics-history";
import { counterRate, counterShare, gapsBetween, stepFor } from "./metrics-series";

const STEP = 10_000;
const window = { from: 0, to: 60_000 };

const row = (metric: string, bucket: number, value: number, extra: Partial<MetricRow> = {}): MetricRow => ({
  metric, machine: "m1", series: "c1", state: "", direction: "", device: "", operation: "", bucket, value, ...extra,
});

describe("counter rates", () => {
  it("counts a restarted counter as no growth, never as negative growth", () => {
    const rows = [0, 10, 20, 30, 40].map((bucket, index) => row("container.cpu.usage.total", bucket, [1e9, 3e9, 5e9, 1e9, 3e9][index] ?? 0));
    const points = servicePoints(rows, window, STEP);
    const rates = points.map((point) => point.cpu);
    expect(rates).toEqual([0.2, 0.2, 0, 0.2]);
    expect(rates.every((rate) => rate === undefined || rate >= 0)).toBe(true);
  });

  it("adds the growth of every series, so a replaced container does not zero the Service", () => {
    const rows = [
      row("container.network.io.usage.rx_bytes", 0, 100, { series: "old" }),
      row("container.network.io.usage.rx_bytes", 10, 600, { series: "old" }),
      row("container.network.io.usage.rx_bytes", 10, 0, { series: "new" }),
      row("container.network.io.usage.rx_bytes", 20, 300, { series: "new" }),
    ];
    expect(counterRate(rows.map((entry) => ({ series: entry.series, at: entry.bucket * 1000, value: entry.value }))).get(20_000)).toBe(30);
    expect(servicePoints(rows, window, STEP).map((point) => point.netIn)).toEqual([50, 30]);
  });

  it("adds the growth of every block device, so a Service reading two disks is not read as one", () => {
    const read = (bucket: number, device: string, value: number) => row("container.blockio.io_service_bytes_recursive", bucket, value, { operation: "read", series: `c1|||${device}` });
    const rows = [0, 10, 20].flatMap((bucket, index) => [read(bucket, "8:0", 1e6 + index * 10e6), read(bucket, "253:0", 5e9 + index * 10e6)]);
    expect(servicePoints(rows, window, STEP).map((point) => point.diskRead)).toEqual([2e6, 2e6]);
  });

  it("keeps a series on its own Machine", () => {
    const rows = [
      row("container.cpu.usage.total", 0, 0, { machine: "a" }),
      row("container.cpu.usage.total", 10, 1e9, { machine: "a" }),
      row("container.cpu.usage.total", 10, 9e9, { machine: "b" }),
    ];
    expect(servicePoints(rows, window, STEP).map((point) => point.cpu)).toEqual([0.1]);
  });
});

describe("host points", () => {
  it("measures busy CPU as a share and survives a reset", () => {
    const cpu = (bucket: number, state: string, value: number) => row("system.cpu.time", bucket, value, { state, series: state });
    const rows = [
      cpu(0, "user", 10), cpu(0, "idle", 90),
      cpu(10, "user", 20), cpu(10, "idle", 180),
      cpu(20, "user", 2), cpu(20, "idle", 5),
    ];
    expect(hostPoints(rows, [], window, STEP).map((point) => point.cpuBusy)).toEqual([10 / 100]);
  });

  /** One counter per core and state, each growing by its own amount per bucket, as hostmetrics sends them. */
  const cores = (growth: readonly Readonly<Record<string, number>>[], buckets = [0, 10, 20, 30]) =>
    buckets.flatMap((bucket, index) => growth.flatMap((states, core) =>
      Object.entries(states).map(([state, grow]) => row("system.cpu.time", bucket, 1000 * (core + 1) + index * grow, { state, series: `cpu${core}|${state}` }))));
  const busy = (rows: readonly MetricRow[]) => hostPoints(rows, [], window, STEP).map((point) => point.cpuBusy);

  it("gives a core's busy share across all its states, and none for the bucket with no predecessor", () => {
    expect(busy(cores([{ user: 2, system: 1, idle: 7 }]))).toEqual([0.3, 0.3, 0.3]);
  });

  it("shares the growth of every core, so one pinned core of four is 25% busy", () => {
    const pinned = { user: 10, idle: 0 };
    const idle = { user: 0, idle: 10 };
    expect(busy(cores([pinned, idle, idle, idle]))).toEqual([0.25, 0.25, 0.25]);
  });

  it("does not count a core waiting for IO as busy", () => {
    expect(busy(cores([{ user: 1, idle: 4, wait: 5 }]))).toEqual([0.1, 0.1, 0.1]);
  });

  it("reads network rates by interface and the disk breakdown", () => {
    const net = (bucket: number, direction: string, value: number) => row("system.network.io", bucket, value, { device: "eth0", direction, series: direction });
    const disk: DiskRow[] = [
      { machine: "m1", kind: "total", bucket: 10, value: 1000 },
      { machine: "m1", kind: "free", bucket: 10, value: 500 },
      { machine: "m1", kind: "volumes", bucket: 10, value: 300 },
      { machine: "m1", kind: "mirror_slots", bucket: 10, value: 50 },
      { machine: "m1", kind: "observe_store", bucket: 10, value: 50 },
      { machine: "m1", kind: "other", bucket: 10, value: 100 },
    ];
    const [point] = hostPoints([net(0, "receive", 0), net(10, "receive", 1000), net(0, "transmit", 0), net(10, "transmit", 500)], disk, window, STEP);
    expect(point?.interfaces).toEqual({ eth0: { rx: 100, tx: 50 } });
    expect(point?.disk).toEqual({ total: 1000, free: 500, volumes: 300, mirrors: 50, logsAndMetrics: 50, other: 100 });
  });
});

describe("buckets", () => {
  it("shares one counter's growth between two series", () => {
    const at = (value: number, bucket: number) => ({ series: "s", at: bucket, value });
    expect(counterShare([at(0, 0), at(30, 10_000)], [at(0, 0), at(70, 10_000)]).get(10_000)).toBe(0.3);
  });

  it("widens the step to about 300 buckets in whole sampling periods", () => {
    expect(stepFor({ from: 0, to: 5 * 60_000 })).toBe(10_000);
    expect(stepFor({ from: 0, to: 24 * 3600_000 })).toBe(290_000);
    expect(stepFor({ from: 0, to: 90 * 24 * 3600_000 }) % 10_000).toBe(0);
  });

  it("reports the spans no bucket covers, not the present", () => {
    const gaps = gapsBetween(new Set([0, 10_000, 40_000]), { from: 0, to: 120_000 }, STEP, 120_000);
    expect(gaps).toEqual([{ from: 20_000, to: 40_000 }]);
  });
});
