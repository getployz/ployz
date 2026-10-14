import { describe, expect, it } from "vitest";
import type { RuntimeContainerRecord } from "#/modules/runtime/runtime.collection";
import { servicesOnServers } from "./server-services";

const container = (machineId: string) => ({ id: `c-${machineId}`, displayName: "web", machineId, namespace: "shop-production", kind: "service" });
const globalSlot = (machineId: string, state: string) => ({
  ...container(machineId), kind: "service_container", global: true as const, runtime: { state },
});

describe("servicesOnServers", () => {
  it("names each running Service by its Namespace and name, and skips ones with no container and Ployz's own", () => {
    const services = servicesOnServers([
      { identity: "ployz-system/ingress", containers: [container("m1")] },
      { identity: "shop-production/web", containers: [container("m1"), container("m2")] },
      { identity: "grafana", containers: [container("m1")] },
      { identity: "shop-production/idle", containers: [] },
    ]);
    expect(services.map(({ name, namespace }) => [name, namespace])).toEqual([["web", "shop-production"], ["grafana", null]]);
    expect([...(services[0]?.machineIds ?? [])]).toEqual(["m1", "m2"]);
  });

  it("names the one Server running a Global Service, and none when it runs on more or isn't Global", () => {
    const sole = (containers: RuntimeContainerRecord[]) => servicesOnServers([{ identity: "shop/agent", containers }])[0]?.soleGlobalOn;
    expect(sole([globalSlot("m1", "running"), globalSlot("m2", "exited")])).toBe("m1");
    expect(sole([globalSlot("m1", "running"), globalSlot("m2", "running")])).toBeNull();
    expect(sole([{ ...container("m1"), kind: "service_container", runtime: { state: "running" } }])).toBeNull();
  });
});
