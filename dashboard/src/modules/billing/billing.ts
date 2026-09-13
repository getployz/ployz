import { Schema } from "effect";
import { BILLING_PLANS } from "#/modules/billing/tables";
import type { StoredBillingPlan } from "#/modules/billing/tables";
import type { ServiceRoute } from "#/modules/environment-design/tables";

export const BillingPlan = Schema.Literals(BILLING_PLANS);
export type BillingPlan = typeof BillingPlan.Type;

export const ManagedSubscriptionSnapshot = Schema.Struct({
  activeSubscriptionId: Schema.NullOr(Schema.String),
  currentPlan: Schema.NullOr(BillingPlan),
  productId: Schema.NullOr(Schema.String),
  amount: Schema.NullOr(Schema.Finite),
  currency: Schema.NullOr(Schema.String),
  currentPeriodStart: Schema.NullOr(Schema.Date),
  currentPeriodEnd: Schema.NullOr(Schema.Date),
  hasActiveSubscription: Schema.Boolean,
});
export type ManagedSubscriptionSnapshot =
  typeof ManagedSubscriptionSnapshot.Type;

export const PolarSubscription = Schema.Struct({
  id: Schema.String,
  productId: Schema.String,
  amount: Schema.Finite,
  currency: Schema.String,
  currentPeriodStart: Schema.Date,
  currentPeriodEnd: Schema.Date,
});
export type PolarSubscription = typeof PolarSubscription.Type;

export type LiveManagedSubscriptionSnapshot = ManagedSubscriptionSnapshot & {
  readonly hasUnknownActiveProduct: boolean;
};

export type ProductIds = Readonly<Record<BillingPlan, string>>;

export type CustomDomainCapabilityDenialReason =
  | "free_plan"
  | "inactive_subscription"
  | "expired_subscription"
  | "unknown_product"
  | "provider_unavailable";

export type CustomDomainCapability =
  | { readonly allowed: true }
  | {
      readonly allowed: false;
      readonly reason: CustomDomainCapabilityDenialReason;
    };

const planRank = {
  free: 0,
  solo: 1,
  teams: 2,
} as const satisfies Record<BillingPlan, number>;

export function normalizeStoredBillingPlan(
  plan: StoredBillingPlan | null,
): BillingPlan | null {
  if (plan === "hobby") return "solo";
  if (plan === "pro") return "teams";
  return plan;
}

export function selectManagedSubscriptionSnapshot(
  subscriptions: readonly PolarSubscription[],
  productIds: ProductIds,
): LiveManagedSubscriptionSnapshot {
  const productPlans = new Map<string, BillingPlan>([
    [productIds.free, "free"],
    [productIds.solo, "solo"],
    [productIds.teams, "teams"],
  ]);
  let selected: PolarSubscription | null = null;
  let currentPlan: BillingPlan | null = null;
  let hasUnknownActiveProduct = false;

  for (const subscription of subscriptions) {
    const plan = productPlans.get(subscription.productId);
    if (plan === undefined) {
      hasUnknownActiveProduct = true;
      continue;
    }
    if (currentPlan === null || planRank[plan] >= planRank[currentPlan]) {
      selected = subscription;
      currentPlan = plan;
    }
  }

  return {
    activeSubscriptionId: selected?.id ?? null,
    currentPlan,
    productId: selected?.productId ?? null,
    amount: selected?.amount ?? null,
    currency: selected?.currency ?? null,
    currentPeriodStart: selected?.currentPeriodStart ?? null,
    currentPeriodEnd: selected?.currentPeriodEnd ?? null,
    hasActiveSubscription: selected !== null,
    hasUnknownActiveProduct,
  };
}

export function persistableManagedSubscriptionSnapshot(
  snapshot: LiveManagedSubscriptionSnapshot,
): ManagedSubscriptionSnapshot {
  const { hasUnknownActiveProduct: _hasUnknownActiveProduct, ...persistable } =
    snapshot;
  return persistable;
}

export function previewProration(input: {
  readonly currentAmount: number;
  readonly targetAmount: number;
  readonly currentPeriodStart: Date;
  readonly currentPeriodEnd: Date;
  readonly now: Date;
}) {
  const totalPeriodMs = Math.max(
    input.currentPeriodEnd.getTime() - input.currentPeriodStart.getTime(),
    1,
  );
  const remainingPeriodMs = Math.min(
    Math.max(input.currentPeriodEnd.getTime() - input.now.getTime(), 0),
    totalPeriodMs,
  );
  const remainingRatio = remainingPeriodMs / totalPeriodMs;
  return {
    estimatedDelta: Math.round(
      (input.targetAmount - input.currentAmount) * remainingRatio,
    ),
    remainingRatio,
  };
}

export function evaluateCustomDomainCapability(
  entitlement: LiveManagedSubscriptionSnapshot,
  now: Date,
): CustomDomainCapability {
  if (entitlement.hasUnknownActiveProduct) {
    return { allowed: false, reason: "unknown_product" };
  }
  if (!entitlement.hasActiveSubscription || entitlement.currentPlan === null) {
    return { allowed: false, reason: "inactive_subscription" };
  }
  if (
    entitlement.currentPeriodEnd === null ||
    entitlement.currentPeriodEnd.getTime() <= now.getTime()
  ) {
    return { allowed: false, reason: "expired_subscription" };
  }
  if (entitlement.currentPlan === "free") {
    return { allowed: false, reason: "free_plan" };
  }
  return { allowed: true };
}

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
