import { describe, expect, it } from "vitest";
import { customDomainCapabilityPresentation } from "./ServiceNetworkingSection.presentation";

describe("customDomainCapabilityPresentation", () => {
  it("allows self-hosted custom-domain controls without billing language", () => {
    expect(
      customDomainCapabilityPresentation({
        status: "ready",
        billingMode: "self_hosted",
        currentPlan: null,
        hasActivePaidSubscription: false,
      }),
    ).toEqual({
      status: "allowed",
      canAddOrReplace: true,
      showUpgrade: false,
    });
  });

  it.each(["solo", "teams"] as const)(
    "allows an active hosted %s plan",
    (currentPlan) => {
      expect(
        customDomainCapabilityPresentation({
          status: "ready",
          billingMode: "hosted",
          currentPlan,
          hasActivePaidSubscription: true,
        }),
      ).toEqual({
        status: "allowed",
        canAddOrReplace: true,
        showUpgrade: false,
      });
    },
  );

  it("offers a Solo upgrade only for confirmed hosted Free", () => {
    expect(
      customDomainCapabilityPresentation({
        status: "ready",
        billingMode: "hosted",
        currentPlan: "free",
        hasActivePaidSubscription: false,
      }),
    ).toEqual({
      status: "blocked",
      canAddOrReplace: false,
      message: "Custom domains are available on Solo and Teams.",
      showUpgrade: true,
    });
  });

  it.each([
    [{ status: "loading" as const }, "Checking custom-domain access…"],
    [
      { status: "unavailable" as const },
      "We couldn’t check your plan. Try again.",
    ],
    [
      {
        status: "ready" as const,
        billingMode: "hosted" as const,
        currentPlan: null,
        hasActivePaidSubscription: false,
      },
      "We couldn’t check your plan. Try again.",
    ],
    [
      {
        status: "ready" as const,
        billingMode: "hosted" as const,
        currentPlan: "solo" as const,
        hasActivePaidSubscription: false,
      },
      "We couldn’t check your plan. Try again.",
    ],
  ])("fails closed without optimistic upgrade language", (input, message) => {
    expect(customDomainCapabilityPresentation(input)).toEqual({
      status: "blocked",
      canAddOrReplace: false,
      message,
      showUpgrade: false,
    });
  });
});
