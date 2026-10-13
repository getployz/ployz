import type {
  ContainerId,
  HostPoint,
  MachineId,
  MetricGap,
  MetricHistory,
  MetricHistoryInput,
  MetricHistoryReply,
  MetricMarker,
  ServiceId,
  ServicePoint,
} from "@ployz/sdk";
import { requirements } from "#/modules/runtime/daemon-compat";

type RpcError = MetricHistory["unanswered"][number]["error"];
type Answer = MetricHistory["machines"][number];
type Failure = MetricHistory["unanswered"][number];

const NORMAL = "0123456789abcdef0123456789abcdef";
const ONES = "1".repeat(32);
const TWOS = "2".repeat(32);
const THREES = "3".repeat(32);
const MIB = 1024 * 1024;

// SAFETY: fixture ids are fixed literals already in each brand's shape.
const brand = {
  machine: (value: string) => value as MachineId,
  container: (value: string) => value as ContainerId,
  service: (value: string) => value as ServiceId,
};

type Buckets = { readonly step: number; readonly ts: readonly number[] };

function buckets(input: MetricHistoryInput): Buckets {
  const since = Number(input.since_nanos);
  const until = Number(input.until_nanos);
  const step = Math.max(1, Math.floor((until - since) / 120));
  const ts: number[] = [];
  for (let at = since; at < until; at += step) ts.push(at);
  return { step, ts };
}

const wave = (index: number, base: number, swing: number, seed: number) =>
  Math.round(base + swing * Math.sin(index / 9 + seed));

function servicePoints(ts: readonly number[], seed: number, io: boolean): ServicePoint[] {
  return ts.map((at, index) => {
    const point: ServicePoint = {
      ts: at,
      cpu_usec: wave(index, 300_000, 200_000, seed),
      mem_bytes: wave(index, 200 * MIB, 40 * MIB, seed),
      mem_limit: 512 * MIB,
      rx: wave(index, 40_000, 30_000, seed),
      tx: wave(index, 25_000, 20_000, seed + 1),
    };
    if (io) {
      point.io_r = wave(index, 2 * MIB, 1.5 * MIB, seed + 2);
      point.io_w = wave(index, MIB, 0.8 * MIB, seed + 3);
    }
    return point;
  });
}

const SERVICE_FIELDS = ["cpu_usec", "mem_bytes", "mem_limit", "rx", "tx", "io_r", "io_w"] as const;

/** The top-level series: every reply's bucket summed, as the daemon fans in. */
function sumServicePoints(replies: readonly (readonly ServicePoint[])[]): ServicePoint[] {
  const byTs = new Map<number, ServicePoint>();
  for (const point of replies.flat()) {
    const sum = byTs.get(point.ts) ?? { ts: point.ts };
    for (const field of SERVICE_FIELDS) {
      const value = point[field];
      if (value !== undefined) sum[field] = (sum[field] ?? 0) + value;
    }
    byTs.set(point.ts, sum);
  }
  return [...byTs.values()].sort((left, right) => left.ts - right.ts);
}

const refusal = (machine: string): Failure => ({
  machine_id: brand.machine(machine),
  error: { code: "unsupported", message: "this Server's daemon does not serve metric history", details: { capability: requirements.metrics.capability, target: machine } } satisfies RpcError,
});
const down = (machine: string): Failure => ({
  machine_id: brand.machine(machine),
  error: { code: "unavailable", message: "the Server is not up, so it was not asked", details: null } satisfies RpcError,
});

function serviceReply(machine: string, points: ServicePoint[], step: number, extra: Partial<MetricHistoryReply> = {}): Answer {
  return { machine_id: brand.machine(machine), value: { points: { service: points }, step_nanos: step, gaps: [], ...extra } };
}

function serviceHistory(answers: readonly Answer[], unanswered: readonly Failure[], step: number): MetricHistory {
  const history: MetricHistory = {
    points: { service: sumServicePoints(answers.map((answer) => "service" in answer.value.points ? answer.value.points.service : [])) },
    machines: [...answers],
    unanswered: [...unanswered],
  };
  if (answers.length > 0) history.step_nanos = step;
  return history;
}

function marker(service: string, machine: string, ts: number, kind: "oom" | "restart"): MetricMarker {
  return {
    machine_id: brand.machine(machine),
    container_id: brand.container("c0ffee000000"),
    service_id: brand.service("svc-postgres"),
    service,
    deployment_id: "dep-verify",
    replica: "postgres-1",
    ts,
    kind,
  };
}

function postgres(service: string, { step, ts }: Buckets): MetricHistory {
  const third = Math.floor(ts.length / 3);
  const gapFrom = ts[third];
  const gapTo = ts[2 * third];
  const gaps: MetricGap[] = gapFrom === undefined || gapTo === undefined ? [] : [{ from: gapFrom, to: gapTo, reason: "not_sampled" }];
  const points = servicePoints(ts, 2, true).filter((_, index) => index < third || index >= 2 * third);
  const oomAt = ts[2 * third + 2] ?? ts.at(-1) ?? 0;
  const markers = [marker(service, NORMAL, oomAt, "oom"), marker(service, NORMAL, oomAt + Math.floor(step / 2), "restart")];
  return serviceHistory([serviceReply(NORMAL, points, step, { gaps, markers })], [], step);
}

function serviceFixture(service: string, range: Buckets): MetricHistory {
  const { step, ts } = range;
  switch (service.split("/").at(-1)) {
    case "web":
      return serviceHistory([serviceReply(NORMAL, servicePoints(ts, 0, false), step), serviceReply(ONES, servicePoints(ts, 1.3, false), step)], [], step);
    case "api":
      return serviceHistory([serviceReply(NORMAL, servicePoints(ts, 0.6, false), step)], [refusal(TWOS), down(THREES)], step);
    case "worker":
      return serviceHistory([serviceReply(NORMAL, servicePoints(ts, 2.1, false), step), serviceReply(ONES, [], step)], [], step);
    case "redis":
      return serviceHistory([], [refusal(ONES), refusal(TWOS)], step);
    case "postgres":
      return postgres(service, range);
    default:
      return serviceHistory([serviceReply(NORMAL, [], step)], [], step);
  }
}

function hostPoints(ts: readonly number[]): HostPoint[] {
  const total = 64 * 1024 * MIB;
  const disk = 2_000_000 * MIB;
  return ts.map((at, index) => ({
    ts: at,
    cpu_busy: wave(index, 350, 200, 0) / 1000,
    memory: { total, available: total - wave(index, 24 * 1024 * MIB, 4 * 1024 * MIB, 1) },
    interfaces: {
      eth0: { rx: wave(index, 900_000, 600_000, 0), tx: wave(index, 400_000, 300_000, 1) },
      lo: { rx: 12_000, tx: 12_000 },
      docker0: { rx: wave(index, 300_000, 100_000, 2), tx: wave(index, 280_000, 100_000, 3) },
    },
    disk: { total: disk, free: disk - 800_000 * MIB - index * 100 * MIB, volumes: 500_000 * MIB + index * 100 * MIB, mirror_slots: 120_000 * MIB, observe_store: 30_000 * MIB, other: 150_000 * MIB },
  }));
}

function machineFixture(machine: string, { step, ts }: Buckets): MetricHistory {
  if (machine === NORMAL) {
    const points = hostPoints(ts);
    return {
      step_nanos: step,
      points: { machine: points },
      machines: [{ machine_id: brand.machine(machine), value: { points: { machine: points }, step_nanos: step, gaps: [] } }],
      unanswered: [],
    };
  }
  return { points: { machine: [] }, machines: [], unanswered: [machine === TWOS ? refusal(machine) : down(machine)] };
}

/** A canned metric history for the verify dashboard, which has no real Servers to ask. Same input, same reply. */
export function verifyMetricHistory(input: MetricHistoryInput): MetricHistory {
  const range = buckets(input);
  return input.subject.kind === "service" ? serviceFixture(input.subject.service, range) : machineFixture(input.subject.machine_id, range);
}
