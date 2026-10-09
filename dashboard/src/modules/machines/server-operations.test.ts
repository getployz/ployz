import type { MachineId, RuntimeWatchView } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { cleanPlan, drainPlan, removePlan, volumeLabels } from "#/modules/machines/server-operations.server";

const here = "a".repeat(32);
const there = "b".repeat(32);

type Slot = {
  machine: string;
  mode?: "replicated" | "global";
  volumes?: number;
  mounts?: Record<string, string>;
  state?: string;
  at?: number;
};

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
      resolved_spec: {
        mode: { mode: slot.mode ?? "replicated" },
        volumes: [
          ...Array.from({ length: slot.volumes ?? 0 }, () => ({ reference: "v", source: { kind: "bind" } })),
          ...Object.keys(slot.mounts ?? {}).map((docker, index) => ({ reference: `r${index}`, source: { kind: "ordinary", name: docker } })),
        ],
        mounts: Object.values(slot.mounts ?? {}).map((target, index) => ({ volume: `r${index}`, target })),
      },
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
      ["deletes_volume", "on fra-1"],
      ["deletes_volume", "on fra-2"],
    ]);
    expect(plan.confirmDataLoss).toEqual([volume(here, "old_a"), volume(there, "old_b")]);
  });

  it("a clean names each Volume by the Services that mount it and its Server, never by its Docker name", () => {
    const plan = cleanPlan(frame({
      "old/db": [{ machine: here, mounts: { old_vol_a: "/var/lib/postgresql" } }, { machine: there, mounts: { old_vol_a: "/data" } }],
      "old/backup": [{ machine: here, state: "exited", mounts: { old_vol_a: "/backup" } }],
    }), "old", [volume(here, "old_vol_a"), volume(there, "old_vol_a"), volume(here, "old_vol_b"), volume("c".repeat(32), "old_vol_c")]);

    expect(plan.effects.filter(({ kind }) => kind === "deletes_volume").map(({ name, node }) => [node, name])).toEqual([
      [`${here}/old_vol_a`, "used by backup at /backup, db at /var/lib/postgresql on fra-1"],
      [`${here}/old_vol_b`, "on fra-1"],
      [`${there}/old_vol_a`, "used by db at /data on fra-2"],
      [`${"c".repeat(32)}/old_vol_c`, "on a Server Cloud can't see"],
    ]);
  });

  it("removing a Server always destroys the Server, and each Volume it confirms losing", () => {
    expect(removePlan(here, "fra-1", []).effects).toEqual([
      { kind: "removes_server", name: "fra-1", node: here, path: `servers/${here}` },
    ]);
    expect(removePlan(here, "fra-1", [volume(here, "data")]).preview).toEqual({ server: here, reset: true, volumes: [`${here}/data`] });
    expect(removePlan(here, "fra-1", null)).toMatchObject({
      preview: { server: here, reset: false, volumes: [] },
      effects: [{ kind: "removes_server", name: "fra-1" }],
    });
  });

  it("a removal names each Volume it deletes as Ployz does, keeping the Docker name as its identity", () => {
    const docker = "shop-production_vol-1";
    const plan = removePlan(here, "fra-1", [volume(here, docker)], new Map([[docker, "cache-data"]]));
    expect(plan.effects[1]).toEqual({ kind: "deletes_volume", name: "cache-data", node: `${here}/${docker}`, path: `volumes/${here}/${docker}` });
  });

  it("a Volume's name is qualified only as far as it must be to be unique", () => {
    const owner = (docker: string, project: string, environment: string, volume: string) => ({ dockerName: docker, project, environment, volume });
    expect(volumeLabels([
      owner("a", "shop", "production", "cache-data"),
      owner("b", "shop", "production", "uploads"),
      owner("c", "shop", "staging", "uploads"),
      owner("d", "blog", "staging", "uploads"),
    ])).toEqual(new Map([["a", "cache-data"], ["b", "production/uploads"], ["c", "shop/staging/uploads"], ["d", "blog/staging/uploads"]]));
  });
});
