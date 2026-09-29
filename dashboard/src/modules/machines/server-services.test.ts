import { describe, expect, it } from "vitest";
import { servicesOnServers } from "./server-services";

const container = (machineId: string) => ({ id: `c-${machineId}`, displayName: "web", machineId, namespace: "shop-production", kind: "service" });

describe("servicesOnServers", () => {
  it("names each running Service by its Namespace and name, and skips ones with no container", () => {
    const services = servicesOnServers([
      { identity: "shop-production/web", containers: [container("m1"), container("m2")] },
      { identity: "grafana", containers: [container("m1")] },
      { identity: "shop-production/idle", containers: [] },
    ]);
    expect(services.map(({ name, namespace }) => [name, namespace])).toEqual([["web", "shop-production"], ["grafana", null]]);
    expect([...(services[0]?.machineIds ?? [])]).toEqual(["m1", "m2"]);
  });
});
