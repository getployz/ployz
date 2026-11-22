import { describe, expect, it } from "vitest";
import { getManagedServiceExports } from "./managed-service-exports";

const service = {
  id: "11111111-1111-4111-8111-111111111111",
  environmentId: "22222222-2222-4222-8222-222222222222",
  privateDns: "api",
  environmentSlug: "production",
};

describe("managed service exports", () => {
  it("exports platform connection values by default", () => {
    const exports = getManagedServiceExports(service);

    expect(exports).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          key: "PLOYZ_PRIVATE_DOMAIN",
          value: "api.internal",
        }),
        expect.objectContaining({
          key: "PLOYZ_SERVICE_NAME",
          value: "api",
        }),
        expect.objectContaining({
          key: "PORT",
          value: "8080",
        }),
      ]),
    );
  });
});
