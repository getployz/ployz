import { Effect } from "effect";
import {
  evaluateCustomDomainCapability,
  routeMutationRequiresCustomDomainCapability,
  type CustomDomainCapability,
} from "#/modules/billing/billing";
import { getActiveManagedSubscriptionSnapshot } from "#/modules/billing/billing.server";
import { Polar } from "#/modules/billing/polar-provider.server";

export const CUSTOM_DOMAIN_CAPABILITY_TIMEOUT_MS = 5_000;

export const getCustomDomainCapability = Effect.fn(
  "Billing.getCustomDomainCapability",
)(function* (
  organizationId: string,
  timeoutMs = CUSTOM_DOMAIN_CAPABILITY_TIMEOUT_MS,
) {
  const polar = yield* Polar;
  if (polar.mode === "self_hosted") {
    return { allowed: true } as const satisfies CustomDomainCapability;
  }

  return yield* getActiveManagedSubscriptionSnapshot(organizationId).pipe(
    Effect.timeout(timeoutMs),
    Effect.map((snapshot) =>
      evaluateCustomDomainCapability(snapshot, new Date()),
    ),
    Effect.catch(() =>
      Effect.succeed({
        allowed: false,
        reason: "provider_unavailable",
      } as const satisfies CustomDomainCapability),
    ),
  );
});

export { routeMutationRequiresCustomDomainCapability };
export type {
  CustomDomainCapability,
  CustomDomainCapabilityDenialReason,
} from "#/modules/billing/billing";
