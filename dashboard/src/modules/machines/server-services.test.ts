import { describe, expect, it } from "vitest";
import { servicesOnServers } from "./server-services";

const container = (machineId: string) => ({ id: `c-${machineId}`, displayName: "web", machineId, projectName: "shop-production", kind: "service" });

describe("servicesOnServers", () => {
  it("joins each running Service to its Cloud Service by namespace and private DNS name", () => {
    const [web, cli] = servicesOnServers(
      [
        { identity: "shop-production/web", containers: [container("m1"), container("m2")] },
        { identity: "ops/grafana", containers: [container("m1")] },
        { identity: "shop-production/idle", containers: [] },
      ],
      [{ name: "Web", privateDns: "web", environmentSlug: "shop-production" }],
    );
    expect(web).toMatchObject({ name: "Web", cloud: { name: "Web" } });
    expect([...(web?.machineIds ?? [])]).toEqual(["m1", "m2"]);
    expect(cli).toMatchObject({ identity: "ops/grafana", name: "grafana", cloud: null });
  });
});
