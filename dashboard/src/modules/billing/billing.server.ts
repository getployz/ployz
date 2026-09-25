import "@tanstack/react-start/server-only";

import { eq, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { user } from "#/modules/identity/tables";

import {
  selectManagedSubscriptionSnapshot,
  type ManagedSubscriptionSnapshot,
} from "#/modules/billing/billing";
import { Polar } from "#/modules/billing/polar-provider.server";
import {
  getOrganizationForUserBySlug,
} from "#/modules/environment-design/workspace-repository.server";
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

/** Reads the synced billing row, never Polar, so a Polar outage cannot block callers. */
export const hasCachedActiveSubscription = Effect.fn(
  "Billing.hasCachedActiveSubscription",
)(function* (organizationId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ active: schemaOrganizationBillingState.hasActiveSubscription })
    .from(schemaOrganizationBillingState)
    .where(eq(schemaOrganizationBillingState.organizationId, organizationId))
    .limit(1);
  return rows[0]?.active ?? false;
});

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
    hasActiveSubscription: snapshot.hasActiveSubscription,
    syncedAt,
    ...sourceOrdering,
  };

  yield* database.drizzle
    .insert(schemaOrganizationBillingState)
    .values({ organizationId, ...values })
    .onConflictDoUpdate({
      target: schemaOrganizationBillingState.organizationId,
      set: values,
      ...updateOrdering,
    });

  return snapshot;
});

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

export const getBillingState = Effect.fn("Billing.getState")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  yield* requireHostedPolar();
  const organization = yield* getAuthorizedBillingScope(
    actor,
    input.organizationSlug,
  );
  return {
    hasActiveSubscription: yield* hasCachedActiveSubscription(organization.id),
  };
});

export const createEmbeddedCheckout = Effect.fn("Billing.createCheckout")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const polar = yield* requireHostedPolar();
    const organization = yield* getAuthorizedBillingScope(
      actor,
      input.organizationSlug,
    );
    const profile = yield* getBillingUser(actor.userId);
    const config = yield* AppConfig;
    return yield* polar.createCheckout({
      successUrl: config.polarSuccessUrl,
      embedOrigin: config.app.url.origin,
      externalCustomerId: profile.id,
      customerEmail: profile.email,
      customerName: profile.name,
      referenceId: organization.id,
    });
  },
);
