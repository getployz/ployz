import { Effect } from "effect";
import { hasCachedActiveSubscription } from "#/modules/billing/billing.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import type { ServiceRoute } from "#/modules/environment-design/tables";

/** Self-hosted Cloud always allows custom domains; hosted needs an active
 * subscription, read from the cached billing row so a Polar outage cannot block edits. */
export const customDomainsAllowed = Effect.fn("Billing.customDomainsAllowed")(
  function* (organizationId: string) {
    const polar = yield* Polar;
    if (polar.mode === "self_hosted") return true;
    return yield* hasCachedActiveSubscription(organizationId);
  },
);

/** Only added or retargeted routes need the capability; removal never does. */
export function routeMutationRequiresCustomDomainCapability(
  previous: readonly ServiceRoute[],
  next: readonly ServiceRoute[],
) {
  return next.some(
    (route) =>
      !previous.some(
        (current) =>
          current.hostname === route.hostname &&
          current.targetPort === route.targetPort,
      ),
  );
}
