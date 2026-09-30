import { describe, expect, it } from "vitest";
import { selectManagedSubscriptionSnapshot } from "#/modules/billing/billing";

const productId = "product-pro";

describe("billing policy", () => {
  it("is inactive without a subscription to the configured product", () => {
    expect(
      selectManagedSubscriptionSnapshot(
        [
          {
            id: "sub-retired-teams",
            productId: "product-teams",
            currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
            cancelAtPeriodEnd: false,
          },
        ],
        productId,
      ),
    ).toEqual({
      activeSubscriptionId: null,
      currentPeriodEnd: null,
      cancelAtPeriodEnd: false,
      hasActiveSubscription: false,
    });
  });

  it("selects the configured product's subscription that runs longest", () => {
    const later = new Date("2026-05-01T00:00:00.000Z");
    expect(
      selectManagedSubscriptionSnapshot(
        [
          {
            id: "sub-earlier",
            productId,
            currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
            cancelAtPeriodEnd: false,
          },
          {
            id: "sub-later",
            productId,
            currentPeriodEnd: later,
            cancelAtPeriodEnd: true,
          },
        ],
        productId,
      ),
    ).toEqual({
      activeSubscriptionId: "sub-later",
      currentPeriodEnd: later,
      cancelAtPeriodEnd: true,
      hasActiveSubscription: true,
    });
  });
});
