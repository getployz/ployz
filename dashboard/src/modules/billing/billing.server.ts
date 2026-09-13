import "@tanstack/react-start/server-only";

import { eq, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { user } from "#/modules/identity/tables";

import { sendInngestEvent } from "#/modules/inngest/client";
import { createOrganizationBillingSyncRequestedEvent } from "#/modules/inngest/events";
import {
  normalizeStoredBillingPlan,
  persistableManagedSubscriptionSnapshot,
  previewProration,
  selectManagedSubscriptionSnapshot,
  type BillingPlan,
  type LiveManagedSubscriptionSnapshot,
  type ManagedSubscriptionSnapshot,
} from "#/modules/billing/billing";
import {
  Polar,
  type CreatePolarCheckout,
} from "#/modules/billing/polar-provider.server";
import {
  getOrganizationForUserBySlug,
} from "#/modules/environment-design/workspace-repository.server";
import type { Actor } from "#/modules/identity/actor";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import {
  organizationBillingState as schemaOrganizationBillingState,
} from "#/modules/billing/tables";

type OrganizationBillingStateRow =
  typeof schemaOrganizationBillingState.$inferSelect;

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

export class BillingConflict extends Data.TaggedError("Conflict")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "conflict" as const;
}

export class BillingEventFailure extends Data.TaggedError(
  "BillingEventFailure",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

function toManagedSubscriptionSnapshot(
  state: OrganizationBillingStateRow | null,
): ManagedSubscriptionSnapshot {
  return {
    activeSubscriptionId: state?.activeSubscriptionId ?? null,
    currentPlan: normalizeStoredBillingPlan(state?.currentPlan ?? null),
    productId: state?.productId ?? null,
    amount: state?.amount ?? null,
    currency: state?.currency ?? null,
    currentPeriodStart: state?.currentPeriodStart ?? null,
    currentPeriodEnd: state?.currentPeriodEnd ?? null,
    hasActiveSubscription: state?.hasActiveSubscription ?? false,
  };
}

export const getOrganizationBillingState = Effect.fn(
  "Billing.getOrganizationState",
)(function* (organizationId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select()
    .from(schemaOrganizationBillingState)
    .where(eq(schemaOrganizationBillingState.organizationId, organizationId))
    .limit(1);
  return rows[0] ?? null;
});

export const getCachedManagedSubscriptionSnapshot = Effect.fn(
  "Billing.getCachedSnapshot",
)(function* (organizationId: string) {
  return toManagedSubscriptionSnapshot(
    yield* getOrganizationBillingState(organizationId),
  );
});

export const getActiveManagedSubscriptionSnapshot = Effect.fn(
  "Billing.getActiveSnapshot",
)(function* (organizationId: string) {
  if (organizationId.trim().length === 0) {
    return yield* new BillingValidation({
      message: "organizationId is required",
    });
  }
  const polar = yield* Polar;
  if (polar.mode === "self_hosted") {
    return yield* new BillingValidation({
      message: "Hosted billing is not configured",
    });
  }
  const subscriptions = yield* polar.listActiveSubscriptions(organizationId);
  return selectManagedSubscriptionSnapshot(subscriptions, polar.productIds);
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

  yield* database.drizzle
    .insert(schemaOrganizationBillingState)
    .values({
      organizationId,
      activeSubscriptionId: snapshot.activeSubscriptionId,
      currentPlan: snapshot.currentPlan,
      productId: snapshot.productId,
      amount: snapshot.amount,
      currency: snapshot.currency,
      currentPeriodStart: snapshot.currentPeriodStart,
      currentPeriodEnd: snapshot.currentPeriodEnd,
      hasActiveSubscription: snapshot.hasActiveSubscription,
      syncedAt,
      ...sourceOrdering,
    })
    .onConflictDoUpdate({
      target: schemaOrganizationBillingState.organizationId,
      set: {
        activeSubscriptionId: snapshot.activeSubscriptionId,
        currentPlan: snapshot.currentPlan,
        productId: snapshot.productId,
        amount: snapshot.amount,
        currency: snapshot.currency,
        currentPeriodStart: snapshot.currentPeriodStart,
        currentPeriodEnd: snapshot.currentPeriodEnd,
        hasActiveSubscription: snapshot.hasActiveSubscription,
        syncedAt,
        ...sourceOrdering,
      },
      ...updateOrdering,
    });

  return snapshot;
});

export const ensureOrganizationFreeSubscription = Effect.fn(
  "Billing.ensureFreeSubscription",
)(function* (input: { readonly organizationId: string; readonly userId: string }) {
  const polar = yield* Polar;
  if (polar.mode === "self_hosted") return false;

  const current = yield* getActiveManagedSubscriptionSnapshot(
    input.organizationId,
  );
  if (current.hasActiveSubscription || current.hasUnknownActiveProduct) {
    return false;
  }

  const subscription = yield* polar.createFreeSubscription(input);
  yield* persistOrganizationBillingStateSnapshot(input.organizationId, {
    activeSubscriptionId: subscription.id,
    currentPlan: "free",
    productId: subscription.productId,
    amount: subscription.amount,
    currency: subscription.currency,
    currentPeriodStart: subscription.currentPeriodStart,
    currentPeriodEnd: subscription.currentPeriodEnd,
    hasActiveSubscription: true,
  });
  return true;
});

const requireHostedPolar = Effect.fn("Billing.requireHostedPolar")(
  function* () {
    const polar = yield* Polar;
    if (polar.mode === "self_hosted") {
      return yield* new BillingValidation({
        message: "Hosted billing is not configured",
      });
    }
    return polar;
  },
);

const getAuthorizedBillingScope = Effect.fn("Billing.authorizeScope")(
  function* (actor: Actor, organizationSlug: string) {
    const organization = yield* getOrganizationForUserBySlug(
      actor.userId,
      organizationSlug,
    );
    if (organization === null) {
      return yield* new BillingNotFound({ resource: "Organization" });
    }
    return organization;
  },
);

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

const getTargetPlanPrice = Effect.fn("Billing.getTargetPlanPrice")(
  function* (productId: string, currency: string) {
    const polar = yield* requireHostedPolar();
    const prices = yield* polar.getProductPrices(productId);
    const price = prices.find((candidate) => candidate.currency === currency);
    if (price === undefined) {
      return yield* new BillingNotFound({ resource: "Polar product price" });
    }
    if (price.kind === "unsupported") {
      return yield* new BillingValidation({
        message: "Unsupported product price type",
      });
    }
    return {
      amount: price.kind === "free" ? 0 : price.amount,
      currency: price.currency,
    };
  },
);

export const getBillingState = Effect.fn("Billing.getState")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const organization = yield* getAuthorizedBillingScope(
    actor,
    input.organizationSlug,
  );
  const snapshot = yield* getCachedManagedSubscriptionSnapshot(
    organization.id,
  );
  const polar = yield* Polar;
  return {
    billingMode: polar.mode,
    activeSubscriptionId: snapshot.activeSubscriptionId,
    currentPlan: snapshot.currentPlan,
    hasActiveSubscription: snapshot.hasActiveSubscription,
    hasActivePaidSubscription:
      snapshot.hasActiveSubscription && snapshot.currentPlan !== "free",
  };
});

export const previewSubscriptionPlanChange = Effect.fn(
  "Billing.previewSubscriptionPlanChange",
)(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly plan: BillingPlan },
) {
  const organization = yield* getAuthorizedBillingScope(
    actor,
    input.organizationSlug,
  );
  const subscription = yield* getCachedManagedSubscriptionSnapshot(
    organization.id,
  );
  if (
    !subscription.hasActiveSubscription ||
    subscription.activeSubscriptionId === null ||
    subscription.currentPlan === null ||
    subscription.amount === null ||
    subscription.currency === null ||
    subscription.currentPeriodStart === null ||
    subscription.currentPeriodEnd === null
  ) {
    return yield* new BillingNotFound({ resource: "Active subscription" });
  }
  const polar = yield* requireHostedPolar();
  const targetPrice = yield* getTargetPlanPrice(
    polar.productIds[input.plan],
    subscription.currency,
  );
  const proration = previewProration({
    currentAmount: subscription.amount,
    targetAmount: targetPrice.amount,
    currentPeriodStart: subscription.currentPeriodStart,
    currentPeriodEnd: subscription.currentPeriodEnd,
    now: new Date(),
  });
  return {
    currentPlan: subscription.currentPlan,
    targetPlan: input.plan,
    currency: targetPrice.currency,
    currentAmount: subscription.amount,
    targetAmount: targetPrice.amount,
    ...proration,
    currentPeriodStart: subscription.currentPeriodStart,
    currentPeriodEnd: subscription.currentPeriodEnd,
    prorationBehavior: "invoice" as const,
  };
});

export const updateSubscriptionPlan = Effect.fn("Billing.updatePlan")(
  function* (
    actor: Actor,
    input: { readonly organizationSlug: string; readonly plan: BillingPlan },
  ) {
    const organization = yield* getAuthorizedBillingScope(
      actor,
      input.organizationSlug,
    );
    const subscription = yield* getCachedManagedSubscriptionSnapshot(
      organization.id,
    );
    if (
      !subscription.hasActiveSubscription ||
      subscription.activeSubscriptionId === null
    ) {
      return yield* new BillingNotFound({ resource: "Active subscription" });
    }
    if (subscription.currentPlan === "free" && input.plan !== "free") {
      return yield* new BillingConflict({
        message: "Free to paid upgrades must go through the customer portal",
      });
    }
    const polar = yield* requireHostedPolar();
    const updated = yield* polar.updateSubscriptionPlan({
      subscriptionId: subscription.activeSubscriptionId,
      plan: input.plan,
    });
    yield* sendInngestEvent(
      createOrganizationBillingSyncRequestedEvent({
          organizationId: organization.id,
          reason: "subscription.update",
        }),
    ).pipe(
      Effect.mapError((cause) => new BillingEventFailure({ cause })),
    );
    return updated;
  },
);

export const createEmbeddedCheckout = Effect.fn("Billing.createCheckout")(
  function* (
    actor: Actor,
    input: { readonly organizationSlug: string; readonly plan: BillingPlan },
  ) {
    const organization = yield* getAuthorizedBillingScope(
      actor,
      input.organizationSlug,
    );
    const [subscription, profile] = yield* Effect.all([
      getCachedManagedSubscriptionSnapshot(organization.id),
      getBillingUser(actor.userId),
    ]);
    const polar = yield* requireHostedPolar();
    const config = yield* AppConfig;
    const checkoutInput: CreatePolarCheckout = {
      productId: polar.productIds[input.plan],
      successUrl: config.polarSuccessUrl,
      embedOrigin: config.app.url.origin,
      externalCustomerId: profile.id,
      customerEmail: profile.email,
      customerName: profile.name,
      referenceId: organization.id,
      subscriptionId:
        subscription.currentPlan === "free"
          ? (subscription.activeSubscriptionId ?? undefined)
          : undefined,
    };
    return yield* polar.createCheckout(checkoutInput);
  },
);

export { persistableManagedSubscriptionSnapshot };
export type { LiveManagedSubscriptionSnapshot, ManagedSubscriptionSnapshot };
