import "@tanstack/react-start/server-only";

import { eq, sql } from "drizzle-orm";
import { Data, Effect } from "effect";

import {
  selectManagedSubscriptionSnapshot,
  type ManagedSubscriptionSnapshot,
} from "#/modules/billing/billing";
import { Polar } from "#/modules/billing/polar-provider.server";
import { PostHog } from "#/modules/analytics/posthog.server";
import { requireAccess } from "#/modules/organization/access.server";
import type { Actor } from "#/modules/identity/actor";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import {
  organizationBillingState as schemaOrganizationBillingState,
} from "#/modules/billing/tables";

export class BillingValidation extends Data.TaggedError("SchemaError")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "validation" as const;
}

export class BillingNotFound extends Data.TaggedError("NotFound")<{
  readonly resource: string;
}> {
  readonly publicErrorCategory = "not-found" as const;
}

/** Billing exists only on Ployz-hosted Cloud; self-hosted reads as not found. */
const requireHostedPolar = Effect.fn("Billing.requireHostedPolar")(
  function* () {
    const polar = yield* Polar;
    if (polar.mode === "self_hosted") {
      return yield* new BillingNotFound({ resource: "Billing" });
    }
    return polar;
  },
);

export const getActiveManagedSubscriptionSnapshot = Effect.fn(
  "Billing.getActiveSnapshot",
)(function* (organizationId: string) {
  if (organizationId.trim().length === 0) {
    return yield* new BillingValidation({
      message: "organizationId is required",
    });
  }
  const polar = yield* requireHostedPolar();
  const subscriptions = yield* polar.listActiveSubscriptions(organizationId);
  return selectManagedSubscriptionSnapshot(subscriptions, polar.productId);
});

export const persistOrganizationBillingStateSnapshot = Effect.fn(
  "Billing.persistSnapshot",
)(function* (
  organizationId: string,
  snapshot: ManagedSubscriptionSnapshot,
  sourceUpdatedAt: Date | null = null,
) {
  const database = yield* Database;
  const syncedAt = new Date();
  const sourceOrdering = sourceUpdatedAt === null ? {} : { sourceUpdatedAt };
  const updateOrdering =
    sourceUpdatedAt === null
      ? {}
      : {
          setWhere: sql`${schemaOrganizationBillingState.sourceUpdatedAt} IS NULL OR ${schemaOrganizationBillingState.sourceUpdatedAt} <= excluded.source_updated_at`,
        };
  const values = {
    activeSubscriptionId: snapshot.activeSubscriptionId,
    currentPeriodEnd: snapshot.currentPeriodEnd,
    cancelAtPeriodEnd: snapshot.cancelAtPeriodEnd,
    hasActiveSubscription: snapshot.hasActiveSubscription,
    syncedAt,
    ...sourceOrdering,
  };

  const saved = yield* database.drizzle
    .insert(schemaOrganizationBillingState)
    .values({ organizationId, ...values })
    .onConflictDoUpdate({
      target: schemaOrganizationBillingState.organizationId,
      set: values,
      ...updateOrdering,
    })
    .returning({ organizationId: schemaOrganizationBillingState.organizationId });
  // Every path to the billing row passes here (webhooks, checkout, the nightly reconcile); a stale snapshot saves nothing.
  if (saved.length > 0) {
    yield* (yield* PostHog).identifyOrganization(organizationId, {
      pro: snapshot.hasActiveSubscription,
      cancel_at_period_end: snapshot.cancelAtPeriodEnd,
    });
  }

  return snapshot;
});

export const getBillingState = Effect.fn("Billing.getState")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  yield* requireHostedPolar();
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      hasActiveSubscription: schemaOrganizationBillingState.hasActiveSubscription,
      currentPeriodEnd: schemaOrganizationBillingState.currentPeriodEnd,
      cancelAtPeriodEnd: schemaOrganizationBillingState.cancelAtPeriodEnd,
    })
    .from(schemaOrganizationBillingState)
    .where(eq(schemaOrganizationBillingState.organizationId, organization.id))
    .limit(1);
  return (
    rows[0] ?? {
      hasActiveSubscription: false,
      currentPeriodEnd: null,
      cancelAtPeriodEnd: false,
    }
  );
});

/**
 * Whether this Cloud runs web analytics. Ployz-hosted Cloud runs it for every Organization; a Self-hosted Cloud runs
 * it when its operator sets ANALYTICS_ENABLED.
 */
export const analyticsPlan = Effect.fn("Billing.analyticsPlan")(function* () {
  const polar = yield* Polar;
  if (polar.mode === "self_hosted") return (yield* AppConfig).analyticsEnabled ? "self-host-on" as const : "self-host-off" as const;
  return "cloud" as const;
});

/** Checkout and the portal come back to the Organization's billing page. */
const billingPage = (appUrl: URL, organizationSlug: string) =>
  new URL(`/cloud/${encodeURIComponent(organizationSlug)}/~/billing`, appUrl).href;

/** The paying user's Polar portal, as the billing page's "Manage billing" opens it. */
export const createCustomerPortal = Effect.fn("Billing.createPortal")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const polar = yield* requireHostedPolar();
    yield* requireAccess(actor, input.organizationSlug, "operate");
    const config = yield* AppConfig;
    return yield* polar.createCustomerPortal({
      externalCustomerId: actor.userId,
      returnUrl: billingPage(config.app.url, input.organizationSlug),
    });
  },
);
