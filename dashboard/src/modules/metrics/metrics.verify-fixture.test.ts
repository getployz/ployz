import type { MachineId, MetricSubject } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { serverMetrics, serviceMetrics } from "./metrics-model";
import { verifyMetricHistory } from "./metrics.verify-fixture";

const window = { from: 1_700_000_000_000, to: 1_700_003_600_000 };
const nanos = (ms: number) => (BigInt(ms) * 1_000_000n).toString();
const names = new Map<string, string>();

const read = (subject: MetricSubject) => ({
  type: "history" as const,
  window,
  at: window.to,
  history: verifyMetricHistory({ subject, since_nanos: nanos(window.from), until_nanos: nanos(window.to) }),
});
const service = (name: string) => serviceMetrics(read({ kind: "service", service: `shop-production/${name}` }), names);
// SAFETY: test ids are 32 hex digits.
const server = (id: string) => serverMetrics(read({ kind: "machine", machine_id: id as MachineId }), names);

describe("verifyMetricHistory", () => {
  it("is deterministic, with about 120 buckets across the window", () => {
    const input = { subject: { kind: "service", service: "ns/web" } as const, since_nanos: nanos(window.from), until_nanos: nanos(window.to) };
    expect(verifyMetricHistory(input)).toEqual(verifyMetricHistory(input));
    expect(service("web").all.cpu).toHaveLength(120);
  });

  it("gives each seeded Service its state", () => {
    const web = service("web");
    expect(web.availability).toEqual({ type: "normal" });
    expect(web.byServer).toHaveLength(2);

    const api = service("api").availability;
    expect(api.type).toBe("mixed");
    expect(api.type === "mixed" && [api.needsUpgrade.map((ref) => ref.id), api.unanswered.map((ref) => ref.id)])
      .toEqual([["2".repeat(32)], ["3".repeat(32)]]);

    const worker = service("worker");
    expect(worker.availability).toEqual({ type: "normal" });
    expect(worker.byServer).toHaveLength(1);
    expect(worker.all.cpu).toHaveLength(120);
    expect(worker.allGaps).toEqual([]);

    expect(service("redis").availability.type).toBe("needs_upgrade");
    expect(service("unknown").availability).toEqual({ type: "no_readings" });
  });

  it("gives postgres a gap and an OOM followed by a restart", () => {
    const postgres = service("postgres");
    expect(postgres.availability).toEqual({ type: "normal" });
    expect(postgres.allGaps).toHaveLength(1);
    expect(postgres.markers.map((marker) => marker.kind)).toEqual(["oom", "restart"]);
    expect(postgres.all.diskRead.length).toBeGreaterThan(0);
  });

  it("gives each seeded Server its state", () => {
    const normal = server("0123456789abcdef0123456789abcdef");
    expect(normal.availability).toEqual({ type: "normal" });
    expect(normal.interfaces.map((iface) => iface.name)).toEqual(["eth0"]);
    expect(normal.gaps).toHaveLength(1);
    expect(server("4".repeat(32)).availability).toEqual({ type: "no_readings" });
    expect(normal.disk[0]).toMatchObject({ volumes: expect.any(Number), mirrors: expect.any(Number), logsAndMetrics: expect.any(Number), other: expect.any(Number) });
    expect(server("1".repeat(32)).availability.type).toBe("unanswered");
    expect(server("2".repeat(32)).availability.type).toBe("needs_upgrade");
    expect(server("f".repeat(32)).availability.type).toBe("unanswered");
  });
});
