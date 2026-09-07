import { describe, expect, it } from "vitest";
import { Effect } from "effect";
import type { PolarSubscription } from "#/modules/billing/billing";
import {
  getCustomDomainCapability,
  routeMutationRequiresCustomDomainCapability,
} from "#/modules/billing/custom-domain-capability";
import {
  Polar,
  PolarFailure,
  type PolarService,
} from "#/modules/billing/polar-provider.server";

const productIds = {
  free: "product-free",
  solo: "product-solo",
  teams: "product-teams",
} as const;

function hosted(
  subscriptions: Effect.Effect<readonly PolarSubscription[], PolarFailure>,
): PolarService {
  return {
    mode: "hosted",
    productIds,
    listActiveSubscriptions: () => subscriptions,
    createFreeSubscription: () => Effect.die("unused"),
    getProductPrices: () => Effect.die("unused"),
    updateSubscriptionPlan: () => Effect.die("unused"),
    createCheckout: () => Effect.die("unused"),
  };
}

function runCapability(service: PolarService, timeoutMs = 5_000) {
  return Effect.runPromise(
    getCustomDomainCapability("org-1", timeoutMs).pipe(
      Effect.provideService(Polar, service),
    ),
  );
}

const paidSubscription = {
  id: "sub-solo",
  productId: productIds.solo,
  amount: 900,
  currency: "usd",
  currentPeriodStart: new Date("2099-01-01T00:00:00.000Z"),
  currentPeriodEnd: new Date("2099-02-01T00:00:00.000Z"),
};

describe("custom-domain capability", () => {
  it("allows self-hosted use without a provider request", async () => {
    await expect(runCapability({ mode: "self_hosted" })).resolves.toEqual({
      allowed: true,
    });
  });

  it("allows only a current live Solo or Teams subscription", async () => {
    await expect(
      runCapability(hosted(Effect.succeed([paidSubscription]))),
    ).resolves.toEqual({ allowed: true });

    await expect(
      runCapability(
        hosted(
          Effect.succeed([
            { ...paidSubscription, productId: productIds.free },
          ]),
        ),
      ),
    ).resolves.toEqual({ allowed: false, reason: "free_plan" });
  });

  it("fails closed on provider errors and timeouts", async () => {
    await expect(
      runCapability(
        hosted(
          Effect.fail(
            new PolarFailure({
              operation: "list active subscriptions",
              code: "request_failed",
              retriable: true,
              message: "Polar list active subscriptions failed.",
              cause: new Error("offline"),
            }),
          ),
        ),
      ),
    ).resolves.toEqual({ allowed: false, reason: "provider_unavailable" });
    await expect(
      runCapability(hosted(Effect.never), 1),
    ).resolves.toEqual({ allowed: false, reason: "provider_unavailable" });
  });

  it("requires a refresh for add or replacement but not unchanged/removal", () => {
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
