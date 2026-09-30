import { Effect, Schedule } from "effect";
import {
  getActiveManagedSubscriptionSnapshot,
  getAuthorizedBillingScope,
  hasCachedActiveSubscription,
  persistOrganizationBillingStateSnapshot,
} from "#/modules/billing/billing.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import type { Actor } from "#/modules/identity/actor";

/** Self-hosted Cloud always allows custom domains; hosted needs an active
 * subscription, read from the cached billing row so a Polar outage cannot block edits. */
export const customDomainsAllowed = Effect.fn("Billing.customDomainsAllowed")(
  function* (organizationId: string) {
    const polar = yield* Polar;
    if (polar.mode === "self_hosted") return true;
    return yield* hasCachedActiveSubscription(organizationId);
  },
);

/** Whether a member's Organization may add custom domains, so the dashboard can offer Pro before the Store refuses. */
export const getCustomDomainsAllowed = Effect.fn("Billing.getCustomDomainsAllowed")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const organization = yield* getAuthorizedBillingScope(actor, input.organizationSlug);
    return yield* customDomainsAllowed(organization.id);
  },
);

/**
 * Right after a checkout succeeds: reads Pro from Polar and saves it, so the dashboard need not wait for the webhook.
 * Polar can create the subscription a moment after checkout succeeds, so it asks for about ten seconds.
 */
export const syncCustomDomainCapability = Effect.fn("Billing.syncCustomDomainCapability")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const organization = yield* getAuthorizedBillingScope(actor, input.organizationSlug);
    const polar = yield* Polar;
    if (polar.mode === "self_hosted") return true;
    const snapshot = yield* getActiveManagedSubscriptionSnapshot(organization.id).pipe(
      Effect.repeat({ schedule: Schedule.spaced("1 second"), times: 9, until: (next) => next.hasActiveSubscription }),
    );
    // An inactive read is left to the webhook: saving it could overwrite a Pro the webhook just recorded.
    if (!snapshot.hasActiveSubscription) return false;
    yield* persistOrganizationBillingStateSnapshot(organization.id, snapshot);
    return true;
  },
);
