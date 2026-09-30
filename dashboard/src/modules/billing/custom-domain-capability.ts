import { Effect } from "effect";
import { BillingNotFound, hasCachedActiveSubscription } from "#/modules/billing/billing.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import type { Actor } from "#/modules/identity/actor";
import { getOrganizationForUserBySlug } from "#/modules/organization/organization-state.server";

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
    const organization = yield* getOrganizationForUserBySlug(actor.userId, input.organizationSlug);
    if (organization === null) return yield* new BillingNotFound({ resource: "Organization" });
    return yield* customDomainsAllowed(organization.id);
  },
);
