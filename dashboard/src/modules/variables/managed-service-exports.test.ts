import { describe, expect, it } from "vitest";
import { getManagedServiceExports } from "./managed-service-exports";

const service = {
  id: "11111111-1111-4111-8111-111111111111",
  environmentId: "22222222-2222-4222-8222-222222222222",
  lineageId: "33333333-3333-4333-8333-333333333333",
  name: "API Service",
  slug: "api-service",
  privateDns: "api",
  environmentSlug: "production",
  publicDomain: null,
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
          key: "PLOYZ_SERVICE_NAME",
          value: "api",
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

it("lists PLOYZ_PUBLIC_DOMAIN only for a Service with a public domain", () => {
  expect(getManagedServiceExports(service).some((row) => row.key === "PLOYZ_PUBLIC_DOMAIN")).toBe(false);
  expect(getManagedServiceExports({ ...service, publicDomain: "api.example.test" }).find((row) => row.key === "PLOYZ_PUBLIC_DOMAIN")?.value)
    .toBe("api.example.test");
});
