import { Effect } from "effect";
import { organizationBillingState } from "#/modules/billing/tables";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { Database } from "#/server/database.server";

/** A Ployz-hosted Cloud's Polar for tests that only read the billing row, never Polar. */
export const hostedPolar: PolarService = {
  mode: "hosted",
  productId: "production",
  listActiveSubscriptions: () => Effect.die("not used"),
  createCheckout: () => Effect.die("not used"),
  setSubscriptionUnits: () => Effect.die("not used"),
  clearPendingUpdate: () => Effect.die("not used"),
  createCustomerPortal: () => Effect.die("not used"),
};

/** The billing row a Production subscription leaves, which is all a hosted Cloud reads to show uptime and analytics. */
export const holdProduction = (organizationId: string, hasActiveSubscription = true) =>
  Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.insert(organizationBillingState).values({
      organizationId,
      hasActiveSubscription,
      ...(hasActiveSubscription && { activeSubscriptionId: "sub-production", currentPeriodEnd: new Date("2099-01-01T00:00:00.000Z") }),
    });
  }).pipe(Effect.orDie);
