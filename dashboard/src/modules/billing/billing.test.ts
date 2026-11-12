import { describe, expect, it } from "vitest";
import {
  billedUnits,
  monthlyPriceUsd,
  planUnitsChange,
  selectManagedSubscriptionSnapshot,
  type PolarSubscription,
} from "#/modules/billing/billing";

const productId = "product-production";

const subscription = (overrides: Partial<PolarSubscription> = {}): PolarSubscription => ({
  id: "sub-production",
  productId,
  currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
  cancelAtPeriodEnd: false,
  units: 2,
  pendingUpdate: null,
  ...overrides,
});

describe("billing policy", () => {
  it("is inactive without a subscription to the configured product", () => {
    expect(
      selectManagedSubscriptionSnapshot(
        [subscription({ id: "sub-retired-teams", productId: "product-teams" })],
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
          subscription({ id: "sub-earlier" }),
          subscription({ id: "sub-later", currentPeriodEnd: later, cancelAtPeriodEnd: true }),
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

describe("Production units", () => {
  it("carries one unit per Server, and the one Polar requires with none", () => {
    expect([0, 1, 2, 3, 7].map(billedUnits)).toEqual([1, 1, 2, 3, 7]);
  });

  it("prices two Servers at $10 a month and $5 for each extra", () => {
    expect([0, 1, 2, 3, 7].map(monthlyPriceUsd)).toEqual([10, 10, 10, 15, 35]);
  });

  it("changes nothing when the units already match", () => {
    expect(planUnitsChange(subscription({ units: 1 }), 0)).toEqual({ kind: "keep" });
    expect(planUnitsChange(subscription({ units: 4 }), 4)).toEqual({ kind: "keep" });
  });

  it("invoices an increase at once", () => {
    expect(planUnitsChange(subscription({ units: 2 }), 3)).toEqual({
      kind: "set",
      units: 3,
      prorationBehavior: "invoice",
    });
  });

  it("schedules a decrease for the next period", () => {
    expect(planUnitsChange(subscription({ units: 5 }), 3)).toEqual({
      kind: "set",
      units: 3,
      prorationBehavior: "next_period",
    });
    expect(planUnitsChange(subscription({ units: 5 }), 0)).toEqual({
      kind: "set",
      units: 1,
      prorationBehavior: "next_period",
    });
  });

  it("leaves a scheduled decrease that already matches the target", () => {
    expect(planUnitsChange(subscription({ units: 5, pendingUpdate: { units: 3 } }), 3)).toEqual({ kind: "keep" });
  });

  it("replaces a scheduled decrease that no longer matches", () => {
    expect(planUnitsChange(subscription({ units: 5, pendingUpdate: { units: 3 } }), 4)).toEqual({
      kind: "set",
      units: 4,
      prorationBehavior: "next_period",
    });
  });

  it("raises the units while a decrease is pending", () => {
    expect(planUnitsChange(subscription({ units: 5, pendingUpdate: { units: 3 } }), 6)).toEqual({
      kind: "set",
      units: 6,
      prorationBehavior: "invoice",
    });
  });

  it("drops a scheduled decrease when the Servers came back", () => {
    expect(planUnitsChange(subscription({ units: 5, pendingUpdate: { units: 3 } }), 5)).toEqual({
      kind: "clear_pending",
    });
  });

  it("leaves a subscription without units or with another scheduled change alone", () => {
    expect(planUnitsChange(subscription({ units: null }), 9)).toEqual({ kind: "keep" });
    expect(planUnitsChange(subscription({ units: 2, pendingUpdate: { units: null } }), 9)).toEqual({ kind: "keep" });
  });
});
