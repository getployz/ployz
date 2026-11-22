import { asReferenceId } from "#/lib/json";
import type { webhooks } from "#/modules/billing/polar-api";
import { organizationBillingSyncRequested } from "#/modules/billing/billing.events";

type PolarSubscriptionWebhookPayload =
  | webhooks.WebhookSubscriptionCreatedPayload
  | webhooks.WebhookSubscriptionUpdatedPayload
  | webhooks.WebhookSubscriptionActivePayload
  | webhooks.WebhookSubscriptionCanceledPayload
  | webhooks.WebhookSubscriptionRevokedPayload
  | webhooks.WebhookSubscriptionUncanceledPayload;

/** The billing sync a Polar subscription webhook asks for: none when its subscription names no Organization. */
export function billingSyncsFromSubscriptionPayload(payload: PolarSubscriptionWebhookPayload) {
  const organizationId = asReferenceId(payload.data.metadata["referenceId"]);
  if (!organizationId) return [];
  return [
    organizationBillingSyncRequested.create({
      organizationId,
      reason: payload.type,
      sourceUpdatedAt: new Date(payload.timestamp).toISOString(),
    }),
  ];
}

/** The billing syncs a customer-state webhook asks for: one per Organization its active subscriptions name. */
export function billingSyncsFromCustomerStatePayload(payload: webhooks.WebhookCustomerStateChangedPayload) {
  const organizationIds = new Set(
    payload.data.active_subscriptions.flatMap((subscription) => asReferenceId(subscription.metadata["referenceId"]) ?? []),
  );
  return Array.from(organizationIds, (organizationId) =>
    organizationBillingSyncRequested.create({
      organizationId,
      reason: payload.type,
      sourceUpdatedAt: new Date(payload.timestamp).toISOString(),
    }));
}
