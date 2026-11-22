import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

/** Sync every Environment's uptime checks with its Applied State, for Services applied before checks existed. */
export const uptimeBackfillRequested = defineEvent({
  name: "uptime/backfill.requested",
  schema: Schema.Struct({}),
});
