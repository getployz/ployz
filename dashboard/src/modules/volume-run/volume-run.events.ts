import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const volumeRunRequested = defineEvent({
  name: "volume/run.requested",
  schema: Schema.Struct({
    organizationId: Schema.String.check(Schema.isNonEmpty()),
    environment: Schema.String,
    volumeId: Schema.String.check(Schema.isNonEmpty()),
    runId: Schema.String.check(Schema.isUUID()),
    /** The kind of the warm lane this run took over, so the run fences that lane's Machines before it plans. */
    took: Schema.optional(Schema.String),
  }),
  id: ({ runId }) => `volume-run-${runId}`,
});

export const volumeRunTaken = defineEvent({
  name: "volume/run.taken",
  schema: Schema.Struct({ runId: Schema.String }),
  id: ({ runId }) => `volume-run-taken-${runId}`,
});
