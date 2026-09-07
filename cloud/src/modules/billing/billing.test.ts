import { describe, expect, it } from "vitest";
import { Option, Schema } from "effect";
import {
  BillingPlan,
  evaluateCustomDomainCapability,
  normalizeStoredBillingPlan,
  previewProration,
  selectManagedSubscriptionSnapshot,
} from "#/modules/billing/billing";

const period = {
  currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
  currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
};

describe("billing policy", () => {
  it("decodes only current billing plans and normalizes stored aliases", () => {
    expect(
      Option.isSome(Schema.decodeUnknownOption(BillingPlan)("teams")),
    ).toBe(true);
    expect(
      Option.isNone(Schema.decodeUnknownOption(BillingPlan)("pro")),
    ).toBe(true);
    expect(normalizeStoredBillingPlan("hobby")).toBe("solo");
    expect(normalizeStoredBillingPlan("pro")).toBe("teams");
  });

  it("selects the highest known active plan and records unknown products", () => {
    const snapshot = selectManagedSubscriptionSnapshot(
      [
        {
          id: "sub-free",
          productId: "product-free",
          amount: 0,
          currency: "usd",
          ...period,
        },
        {
          id: "sub-unknown",
          productId: "product-unknown",
          amount: 4900,
          currency: "usd",
          ...period,
        },
        {
          id: "sub-teams",
          productId: "product-teams",
          amount: 2900,
          currency: "usd",
          ...period,
        },
        {
          id: "sub-solo",
          productId: "product-solo",
          amount: 900,
          currency: "usd",
          ...period,
        },
      ],
      {
        free: "product-free",
        solo: "product-solo",
        teams: "product-teams",
      },
    );

    expect(snapshot).toEqual({
      activeSubscriptionId: "sub-teams",
      currentPlan: "teams",
      productId: "product-teams",
      amount: 2900,
      currency: "usd",
      ...period,
      hasActiveSubscription: true,
      hasUnknownActiveProduct: true,
    });
  });

  it("fails custom-domain entitlement closed for unknown products", () => {
    expect(
      evaluateCustomDomainCapability(
        {
          activeSubscriptionId: "sub-teams",
          currentPlan: "teams",
          productId: "product-teams",
          amount: 2900,
          currency: "usd",
          ...period,
          hasActiveSubscription: true,
          hasUnknownActiveProduct: true,
        },
        new Date("2026-03-15T00:00:00.000Z"),
      ),
    ).toEqual({ allowed: false, reason: "unknown_product" });
  });

  it("clamps proration to the cached period and rounds the amount delta", () => {
    expect(
      previewProration({
        currentAmount: 900,
        targetAmount: 2900,
        currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
        now: new Date("2026-03-16T12:00:00.000Z"),
      }),
    ).toEqual({ estimatedDelta: 1000, remainingRatio: 0.5 });
  });
});
