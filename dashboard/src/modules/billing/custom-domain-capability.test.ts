import { describe, expect, it } from "vitest";
import { routeMutationRequiresCustomDomainCapability } from "#/modules/billing/custom-domain-capability";

describe("custom-domain capability", () => {
  it("requires the capability for add or retarget but not unchanged/removal", () => {
    const routeId = crypto.randomUUID();
    const current = [
      { id: routeId, hostname: "api.example.com", targetPort: 3000 },
    ];
    expect(routeMutationRequiresCustomDomainCapability(current, current)).toBe(false);
    expect(routeMutationRequiresCustomDomainCapability(current, [])).toBe(false);
    expect(
      routeMutationRequiresCustomDomainCapability(current, [
        { id: routeId, hostname: "api.example.com", targetPort: 8080 },
      ]),
    ).toBe(true);
  });
});
