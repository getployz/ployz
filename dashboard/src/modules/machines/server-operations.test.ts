import type { MachineId, RuntimeWatchView } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { cleanPlan, drainPlan, removePlan } from "#/modules/machines/server-operations.server";

const here = "a".repeat(32);
const there = "b".repeat(32);

type Slot = { machine: string; mode?: "replicated" | "global"; volumes?: number; state?: string; at?: number };

const frame = (services: Record<string, Slot[]>) => asTestDouble<RuntimeWatchView>()({
  machines: [{ machine: { id: here, name: "fra-1" } }, { machine: { id: there, name: "fra-2" } }],
  services: Object.entries(services).map(([identity, slots]) => ({
    identity,
    service_id: identity.split("/")[1],
    containers: slots.map((slot) => ({
      kind: "service_container",
      machine_id: slot.machine,
      runtime: { state: slot.state ?? "running" },
      created_at_unix_nanos: slot.at ?? 1,
      resolved_spec: { mode: { mode: slot.mode ?? "replicated" }, volumes: Array.from({ length: slot.volumes ?? 0 }, () => ({})) },
    })),
  })),
});

describe("drainPlan", () => {
  it("moves stateless Services, keeps ones a Volume holds, retires Globals, and destroys only a Global with no other slot", () => {
    const plan = drainPlan(frame({
      "shop/web": [{ machine: here }],
      "shop/db": [{ machine: here, volumes: 1 }],
      "shop/edge": [{ machine: here, mode: "global" }, { machine: there, mode: "global" }],
      "shop/metrics": [{ machine: here, mode: "global" }, { machine: there, mode: "global", state: "exited" }],
      "shop/elsewhere": [{ machine: there }],
      "other/web": [{ machine: here }],
      "ployz-system/proxy": [{ machine: here }],
    }), new Set(["shop", "ployz-system"]), here);

    expect(plan).toEqual({
      subject: `server:${here}`,
      verb: "drain",
      name: "fra-1",
      preview: { server: here, moves: ["shop/web"], stays: ["shop/db"], retires: ["shop/edge", "shop/metrics"] },
      effects: [{ kind: "removes_service", name: "shop/metrics", node: "metrics", path: "services/shop/metrics" }],
      targets: ["shop/db", "shop/edge", "shop/metrics", "shop/web"],
    });
  });

  it("weighs a Service by its newest slot on the Server", () => {
    const plan = drainPlan(frame({
      "shop/web": [{ machine: here, volumes: 1, at: 1 }, { machine: here, at: 2 }],
    }), new Set(["shop"]), here);
    expect(plan?.preview).toEqual({ server: here, moves: ["shop/web"], stays: [], retires: [] });
  });

  it("is null for a Server the frame doesn't hold", () => {
    expect(drainPlan(frame({}), new Set(), "c".repeat(32))).toBeNull();
  });
});

describe("cleanPlan and removePlan", () => {
  const volume = (machine: string, name: string) =>
    ({ kind: "docker_volume" as const, id: { machine_id: machine as MachineId, name: name } });

  it("a clean destroys the Namespace's running Services and every Volume it confirms, in a stable order", () => {
    const plan = cleanPlan(frame({
      "old/web": [{ machine: here }],
      "old/job": [{ machine: there, state: "exited" }],
      "shop/web": [{ machine: here }],
    }), "old", [volume(there, "old_b"), volume(here, "old_a")]);

    expect(plan.preview).toEqual({ namespace: "old", services: ["old/job", "old/web"], volumes: [`${here}/old_a`, `${there}/old_b`] });
    expect(plan.effects.map(({ kind, name }) => [kind, name])).toEqual([
      ["removes_service", "old/web"],
      ["deletes_volume", "old_a"],
      ["deletes_volume", "old_b"],
    ]);
    expect(plan.confirmDataLoss).toEqual([volume(here, "old_a"), volume(there, "old_b")]);
  });

  it("removing a Server always destroys the Server, and each Volume it confirms losing", () => {
    expect(removePlan(here, "fra-1", []).effects).toEqual([
      { kind: "removes_server", name: "fra-1", node: here, path: `servers/${here}` },
    ]);
    expect(removePlan(here, "fra-1", [volume(here, "data")]).preview).toEqual({ server: here, volumes: [`${here}/data`] });
  });
});
