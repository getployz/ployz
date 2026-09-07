import { describe, expect, it } from "vitest";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";

const service = {
  id: "11111111-1111-4111-8111-111111111111",
  environmentId: "22222222-2222-4222-8222-222222222222",
  lineageId: "33333333-3333-4333-8333-333333333333",
  name: "API Service",
  slug: "api-service",
  environmentSlug: "production",
};

describe("managed service exports", () => {
  it("exports platform connection values by default", () => {
    const exports = getManagedServiceExports(service);

    expect(exports).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          key: "PLOYZ_PRIVATE_DOMAIN",
          value: "api-service-production.internal",
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
