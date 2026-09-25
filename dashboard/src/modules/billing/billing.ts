import { Schema } from "effect";

export const ManagedSubscriptionSnapshot = Schema.Struct({
  activeSubscriptionId: Schema.NullOr(Schema.String),
  currentPeriodEnd: Schema.NullOr(Schema.Date),
  hasActiveSubscription: Schema.Boolean,
});
export type ManagedSubscriptionSnapshot =
  typeof ManagedSubscriptionSnapshot.Type;

export const PolarSubscription = Schema.Struct({
  id: Schema.String,
  productId: Schema.String,
  currentPeriodEnd: Schema.Date,
});
export type PolarSubscription = typeof PolarSubscription.Type;

/** Only the one configured product counts; retired products are ignored. */
export function selectManagedSubscriptionSnapshot(
  subscriptions: readonly PolarSubscription[],
  productId: string,
): ManagedSubscriptionSnapshot {
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
  return {
    activeSubscriptionId: selected?.id ?? null,
    currentPeriodEnd: selected?.currentPeriodEnd ?? null,
    hasActiveSubscription: selected !== null,
  };
}
