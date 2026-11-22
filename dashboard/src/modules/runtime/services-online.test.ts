import { describe, expect, it } from "vitest";
import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { servicesOnline } from "./services-online";

const environment = { namespace: "shop", services: [{ slug: "api" }, { slug: "pg" }] };
const record = (slug: string, ...machineIds: string[]): RuntimeServiceRecord => ({
  id: slug, identity: `shop/${slug}`, serviceId: slug, hookContainers: [], observedAt: "2026-10-18T16:08:00Z",
  containers: machineIds.map((machineId) => ({ id: `${slug}-${machineId}`, displayName: slug, machineId, namespace: "shop", kind: "service",
    runtime: { state: "running", health: "healthy" } })),
});

describe("servicesOnline", () => {
  it("counts a Service online only while a Server that is not offline serves it", () => {
    const services = [record("api", "m1", "m2"), record("pg", "m2")];
    expect(servicesOnline(environment, services, "observed", new Set()).label).toBe("2/2 services online");
    expect(servicesOnline(environment, services, "observed", new Set(["m2"])).label).toBe("1/2 services online");
  });
});
