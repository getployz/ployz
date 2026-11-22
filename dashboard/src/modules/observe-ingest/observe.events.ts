import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const observeCrashReceived = defineEvent({
  name: "observe/crash.received",
  schema: Schema.Struct({ organizationId: Schema.String, serviceId: Schema.String }),
});

/** Keyed by the crash row: a retried record sends it again, and Inngest runs it once. */
export const createObserveCrashReceivedEvent = (data: typeof observeCrashReceived.schema.Type, crashEventId: string) =>
  ({ id: `observe-crash-${crashEventId}`, ...observeCrashReceived.create(data) }) as const;

/** A Machine's capability was published: tell it where to push. The 5-minute sweep covers a lost event. */
export const observePushTargetRequested = defineEvent({
  name: "observe/push-target.requested",
  schema: Schema.Struct({}),
});
