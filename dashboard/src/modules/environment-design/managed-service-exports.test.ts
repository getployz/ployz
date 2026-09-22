import { describe, expect, it } from "vitest";
import { getManagedServiceExports, servicePublicDomain } from "#/modules/environment-design/managed-service-exports";

const service = {
  id: "11111111-1111-4111-8111-111111111111",
  environmentId: "22222222-2222-4222-8222-222222222222",
  lineageId: "33333333-3333-4333-8333-333333333333",
  name: "API Service",
  slug: "api-service",
  privateDns: "api",
  environmentSlug: "production",
  routes: [],
  managedHostnames: [],
};

describe("managed service exports", () => {
  it("exports platform connection values by default", () => {
    const exports = getManagedServiceExports(service);

    expect(exports).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          key: "PLOYZ_PRIVATE_DOMAIN",
          value: "api.internal",
          exported: true,
          managed: true,
        }),
        expect.objectContaining({
          key: "PORT",
          value: "3000",
          exported: true,
          managed: true,
        }),
      ]),
    );
  });

  it("keeps service identity values available for typed references", () => {
    const exports = getManagedServiceExports(service);

    expect(exports.map((item) => item.key)).toEqual([
      "PLOYZ_PRIVATE_DOMAIN",
      "PORT",
      "PLOYZ_ENVIRONMENT_NAME",
      "PLOYZ_SERVICE_NAME",
      "PLOYZ_ENVIRONMENT_ID",
      "PLOYZ_SERVICE_ID",
    ]);
  });
});

it("selects the last linked custom domain, then generated domain, regardless of DNS", () => {
  const first = { id: "first", hostname: "first.example.test", targetPort: null };
  const last = { id: "last", hostname: "unresolved.example.test", targetPort: null };
  const config = { routes: [first, last], managedHostnames: [{ prefix: "old", targetPort: null }, { prefix: "api", targetPort: null }] };
  expect(servicePublicDomain(config, "cluster.example.test")).toBe(last.hostname);
  expect(servicePublicDomain({ ...config, routes: [{ ...first, targetPort: 8080 }, last] }, null)).toBe(last.hostname);
  expect(servicePublicDomain({ ...config, routes: [first] }, null)).toBe(first.hostname);
  expect(servicePublicDomain({ ...config, routes: [] }, "cluster.example.test")).toBe("api.cluster.example.test");
  expect(servicePublicDomain({ ...config, routes: [] }, null)).toBeNull();
  expect(servicePublicDomain(service, "cluster.example.test")).toBeNull();
  expect(getManagedServiceExports({ ...service, ...config }).find(row => row.key === "PLOYZ_PUBLIC_DOMAIN")?.value).toBe(last.hostname);
});
