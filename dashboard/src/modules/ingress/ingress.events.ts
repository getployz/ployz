import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

/** A switch of the Cluster's web traffic between its public ports and Cloudflare was requested: its row is written. */
export const ingressDoorsRequested = defineEvent({
  name: "ingress/doors.requested",
  schema: Schema.Struct({ attemptId: Schema.String.check(Schema.isUUID()), organizationId: Schema.String.check(Schema.isNonEmpty()) }),
  id: ({ attemptId }) => `ingress-doors-${attemptId}`,
});

export type IngressDoorsRequestedEventData = typeof ingressDoorsRequested.schema.Type;
