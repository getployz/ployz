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

  it("marks a Global Service and the Servers running a copy of it", () => {
    const service = (containers: RuntimeContainerRecord[]) => servicesOnServers([{ identity: "shop/agent", containers }])[0];
    const agent = service([globalSlot("m1", "running"), globalSlot("m2", "exited")]);
    expect([agent?.global, [...agent?.runningOn ?? []]]).toEqual([true, ["m1"]]);
    const plain = service([{ ...container("m1"), kind: "service_container", runtime: { state: "running" } }]);
    expect([plain?.global, [...plain?.runningOn ?? []]]).toEqual([false, ["m1"]]);
  });
});
