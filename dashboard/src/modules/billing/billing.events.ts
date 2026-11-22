import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

/** What asked for the sync: a Polar webhook's type, a Server joining or leaving, or the nightly reconcile. */
const BillingSyncReason = Schema.Literals([
  "subscription.created",
  "subscription.updated",
  "subscription.active",
  "subscription.canceled",
  "subscription.revoked",
  "subscription.uncanceled",
  "customer.state_changed",
  "server-joined",
  "server-removed",
  "nightly-reconcile",
]);

/** `sourceUpdatedAt` is when Polar changed the subscription: a sync from older data doesn't overwrite newer. */
export const organizationBillingSyncRequested = defineEvent({
  name: "billing/organization-sync.requested",
  schema: Schema.Struct({
    organizationId: Schema.Trim.check(Schema.isMinLength(1)),
    reason: BillingSyncReason,
    sourceUpdatedAt: Schema.optionalKey(Schema.String),
  }),
});
