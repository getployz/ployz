import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const serviceActRequested = defineEvent({
  name: "service/act.requested",
  schema: Schema.Struct({ actId: Schema.String.check(Schema.isUUID()), organizationId: Schema.String.check(Schema.isNonEmpty()) }),
  id: ({ actId }) => `service-act-${actId}`,
});

export type ServiceActRequestedEventData = typeof serviceActRequested.schema.Type;
