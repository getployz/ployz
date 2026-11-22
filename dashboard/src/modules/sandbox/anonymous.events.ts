import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

/** An anonymous agent's Organization is past its token: tear it down one step further. */
export const anonymousTeardownRequested = defineEvent({
  name: "anonymous/teardown.requested",
  schema: Schema.Struct({ organizationId: Schema.NonEmptyString }),
});
