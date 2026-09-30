import { Effect } from "effect";
import { getAuthorizedBillingScope, hasCachedActiveSubscription } from "#/modules/billing/billing.server";
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
export const getCustomDomainCapability = Effect.fn("Billing.getCustomDomainCapability")(
  function* (actor: Actor, input: { readonly organizationSlug: string }) {
    const organization = yield* getAuthorizedBillingScope(actor, input.organizationSlug);
    return yield* customDomainsAllowed(organization.id);
  },
);
