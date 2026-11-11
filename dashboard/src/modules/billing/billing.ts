import { Schema } from "effect";

/** The Production plan bills this many Servers at least, so the first two cost $10 a month. */
export const MINIMUM_UNITS = 2;
export const UNIT_PRICE_USD = 5;

/** How many Servers a Production subscription bills for: one unit per Server, never fewer than the minimum. */
export const billedUnits = (servers: number) => Math.max(MINIMUM_UNITS, servers);

export const monthlyPriceUsd = (servers: number) => billedUnits(servers) * UNIT_PRICE_USD;

export const ManagedSubscriptionSnapshot = Schema.Struct({
  activeSubscriptionId: Schema.NullOr(Schema.String),
  currentPeriodEnd: Schema.NullOr(Schema.Date),
  /** Cancelled in Polar: Production stays until currentPeriodEnd, then ends. */
  cancelAtPeriodEnd: Schema.Boolean,
  hasActiveSubscription: Schema.Boolean,
});
export type ManagedSubscriptionSnapshot =
  typeof ManagedSubscriptionSnapshot.Type;

/** A Polar subscription as the API sends it (snake_case, ISO dates), decoded to our names. */
export const PolarSubscription = Schema.Struct({
  id: Schema.String,
  productId: Schema.String,
  currentPeriodEnd: Schema.DateFromString,
  cancelAtPeriodEnd: Schema.Boolean,
  units: Schema.NullOr(Schema.Finite),
  /** A change Polar applies at the next period, such as a lower unit count. It locks the subscription until then. */
  pendingUpdate: Schema.NullOr(
    Schema.Struct({ units: Schema.NullOr(Schema.Finite) }),
  ),
}).pipe(
  Schema.encodeKeys({
    productId: "product_id",
    currentPeriodEnd: "current_period_end",
    cancelAtPeriodEnd: "cancel_at_period_end",
    pendingUpdate: "pending_update",
  }),
);
export type PolarSubscription = typeof PolarSubscription.Type;

/** Only the one configured product counts; retired products are ignored. */
export function selectManagedSubscription(
  subscriptions: readonly PolarSubscription[],
  productId: string,
): PolarSubscription | null {
  let selected: PolarSubscription | null = null;
  for (const subscription of subscriptions) {
    if (subscription.productId !== productId) continue;
    if (
      selected === null ||
      subscription.currentPeriodEnd > selected.currentPeriodEnd
    ) {
      selected = subscription;
    }
  }
  return selected;
}

export function selectManagedSubscriptionSnapshot(
  subscriptions: readonly PolarSubscription[],
  productId: string,
): ManagedSubscriptionSnapshot {
  const selected = selectManagedSubscription(subscriptions, productId);
  return {
    activeSubscriptionId: selected?.id ?? null,
    currentPeriodEnd: selected?.currentPeriodEnd ?? null,
    cancelAtPeriodEnd: selected?.cancelAtPeriodEnd ?? false,
    hasActiveSubscription: selected !== null,
  };
}

export type UnitsChange =
  | { readonly kind: "keep" }
  /** The target equals the billed units but a scheduled decrease is still pending. */
  | { readonly kind: "clear_pending" }
  | {
      readonly kind: "set";
      readonly units: number;
      readonly prorationBehavior: "invoice" | "next_period";
    };

/**
 * What to tell Polar so the subscription bills `servers` Servers. More Servers are invoiced at once; fewer apply from the
 * next period, so a Server that leaves mid-period was still paid for. A scheduled decrease already at the target is left.
 */
export function planUnitsChange(
  subscription: PolarSubscription,
  servers: number,
): UnitsChange {
  const current = subscription.units;
  const pending = subscription.pendingUpdate;
  if (current === null) return { kind: "keep" };
  if (pending !== null && pending.units === null) return { kind: "keep" };
  const target = billedUnits(servers);
  if (target === current) {
    return pending === null ? { kind: "keep" } : { kind: "clear_pending" };
  }
  if (target > current) {
    return { kind: "set", units: target, prorationBehavior: "invoice" };
  }
  if (pending?.units === target) return { kind: "keep" };
  return { kind: "set", units: target, prorationBehavior: "next_period" };
}
