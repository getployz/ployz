import "@tanstack/react-start/server-only";

import { eq, sql } from "drizzle-orm";
import { Data, Effect, Schedule } from "effect";

import {
  billedUnits,
  planUnitsChange,
  selectManagedSubscription,
  selectManagedSubscriptionSnapshot,
  type ManagedSubscriptionSnapshot,
} from "#/modules/billing/billing";
import { Polar } from "#/modules/billing/polar-provider.server";
import { countOrganizationMachines } from "#/modules/machines/connections.server";
import { PostHog } from "#/modules/analytics/posthog.server";
import { requireAccess } from "#/modules/organization/access.server";
import type { Actor } from "#/modules/identity/actor";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import { user } from "#/modules/identity/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
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

/** Reads the synced billing row, never Polar, so a Polar outage cannot block callers. */
const hasCachedActiveSubscription = Effect.fn("Billing.hasCachedActiveSubscription")(
  function* (organizationId: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({ active: schemaOrganizationBillingState.hasActiveSubscription })
      .from(schemaOrganizationBillingState)
      .where(eq(schemaOrganizationBillingState.organizationId, organizationId))
      .limit(1);
    return rows[0]?.active ?? false;
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
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "org:read");
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
  const servers = yield* countOrganizationMachines(organization.id);
  return {
    ...(rows[0] ?? {
      hasActiveSubscription: false,
      currentPeriodEnd: null,
      cancelAtPeriodEnd: false,
    }),
    servers,
  };
});

/**
 * Brings the Production subscription's units to the Organization's Server count, from what Polar holds now. Run after a
 * Server joins or leaves, and by the nightly reconcile, so it changes nothing when the units already match.
 */
export const syncSubscriptionUnits = Effect.fn("Billing.syncUnits")(
  function* (organizationId: string) {
    const polar = yield* requireHostedPolar();
    const subscription = selectManagedSubscription(
      yield* polar.listActiveSubscriptions(organizationId),
      polar.productId,
    );
    if (subscription === null) return { kind: "keep" } as const;
    const servers = yield* countOrganizationMachines(organizationId);
    const change = planUnitsChange(subscription, servers);
    if (change.kind === "clear_pending") {
      yield* polar.clearPendingUpdate(subscription.id);
    }
    if (change.kind === "set") {
      yield* polar.setSubscriptionUnits({
        subscriptionId: subscription.id,
        units: change.units,
        prorationBehavior: change.prorationBehavior,
      });
    }
    return change;
  },
);

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

const getBillingUser = Effect.fn("Billing.getUser")(function* (userId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: user.id, email: user.email, name: user.name })
    .from(user)
    .where(eq(user.id, userId))
    .limit(1);
  const profile = rows[0];
  if (profile === undefined) {
    return yield* new BillingNotFound({ resource: "User" });
  }
  return profile;
});

/** Opens with one unit per Server, at least the product's minimum of two. */
export const createEmbeddedCheckout = Effect.fn("Billing.createCheckout")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const polar = yield* requireHostedPolar();
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "org:read");
    yield* requireClaimed(organization.id, "billing");
    // Never sell Production twice.
    if (yield* hasCachedActiveSubscription(organization.id)) {
      return yield* new Conflict({ message: "This Organization already holds Production.", userFacing: true });
    }
    const profile = yield* getBillingUser(actor.userId);
    const config = yield* AppConfig;
    const checkout = yield* polar.createCheckout({
      successUrl: `${billingPage(config.app.url, input.organizationSlug)}?checkout_id={CHECKOUT_ID}`,
      embedOrigin: config.app.url.origin,
      externalCustomerId: profile.id,
      customerEmail: profile.email,
      customerName: profile.name,
      referenceId: organization.id,
      units: billedUnits(yield* countOrganizationMachines(organization.id)),
    });
    yield* (yield* PostHog).capture({ userId: actor.userId, organizationId: organization.id, event: "checkout_started" });
    return checkout;
  },
);

/** The paying user's Polar portal, as the billing page's "Manage billing" opens it. */
export const createCustomerPortal = Effect.fn("Billing.createPortal")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "org:read");
    yield* requireClaimed(organization.id, "billing");
    const polar = yield* requireHostedPolar();
    const config = yield* AppConfig;
    return yield* polar.createCustomerPortal({
      externalCustomerId: actor.userId,
      returnUrl: billingPage(config.app.url, input.organizationSlug),
    });
  },
);

/**
 * Right after a checkout succeeds: reads the subscription from Polar and saves it to the billing row, so the dashboard
 * need not wait for the webhook. Polar can create the subscription a moment after checkout succeeds, so it asks for
 * about ten seconds.
 */
export const syncBillingAfterCheckout = Effect.fn("Billing.syncAfterCheckout")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "org:read");
    const snapshot = yield* getActiveManagedSubscriptionSnapshot(organization.id).pipe(
      Effect.repeat({ schedule: Schedule.spaced("1 second"), times: 9, until: (next) => next.hasActiveSubscription }),
    );
    // An inactive read is left to the webhook: saving it could overwrite a subscription the webhook just recorded.
    if (snapshot.hasActiveSubscription) {
      yield* persistOrganizationBillingStateSnapshot(organization.id, snapshot);
      yield* (yield* PostHog).capture({ userId: actor.userId, organizationId: organization.id, event: "subscription_started" });
    }
    return snapshot.hasActiveSubscription;
  },
);
