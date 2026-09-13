import { describe, expect, it } from "vitest";
import { createCanonicalEnvironmentNamespace } from "#/modules/environment-design/workspace-schemas";

describe("createCanonicalEnvironmentNamespace", () => {
  it("combines the stable project slug with the environment slug", () => {
    expect(
      createCanonicalEnvironmentNamespace({
        projectSlug: "payments-api",
        environmentName: "Production West",
      }),
    ).toBe("payments-api-production-west");
  });

  it("produces different production namespaces for different projects", () => {
    expect(
      createCanonicalEnvironmentNamespace({
        projectSlug: "storefront",
        environmentName: "Production",
      }),
    ).not.toBe(
      createCanonicalEnvironmentNamespace({
        projectSlug: "checkout",
        environmentName: "Production",
      }),
    );
  });
});
