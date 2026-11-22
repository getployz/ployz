import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const clusterNameRequested = defineEvent({
  name: "cluster/name.requested",
  schema: Schema.Struct({ organizationId: Schema.String.check(Schema.isNonEmpty()) }),
});

/** Set the Organization's saved Cluster Name on its Cluster. `id` names one request, so the outbox sends it once. */
export const createClusterNameRequestedEvent = ({ id, ...data }: typeof clusterNameRequested.schema.Type & { readonly id: string }) =>
  ({ id, ...clusterNameRequested.create(data) }) as const;
