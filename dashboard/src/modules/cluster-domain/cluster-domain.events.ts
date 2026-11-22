import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const clusterDomainSyncRequested = defineEvent({
  name: "cluster-domain/sync.requested",
  schema: Schema.Struct({ organizationId: Schema.String.check(Schema.isNonEmpty()) }),
});
