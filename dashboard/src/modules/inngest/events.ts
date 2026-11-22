import { Schema } from "effect";
import { eventType, staticSchema } from "inngest";

export const inngestEventEnvelopeFields = {
  id: Schema.optionalKey(Schema.String),
  v: Schema.optionalKey(Schema.String),
  ts: Schema.optionalKey(Schema.Finite.check(Schema.isInt())),
};

export const inngestEventIdentitySchema = Schema.Trim.check(
  Schema.isMinLength(1),
);

export const inngestFailureErrorSchema = Schema.Struct({
  name: Schema.String,
  message: Schema.String,
});

export function inngestFunctionFailedEnvelopeSchema<
  EventSchema extends Schema.Constraint,
>(eventSchema: EventSchema) {
  return Schema.Struct({
    ...inngestEventEnvelopeFields,
    name: Schema.Literal("inngest/function.failed"),
    data: Schema.Struct({
      function_id: inngestEventIdentitySchema,
      run_id: inngestEventIdentitySchema,
      error: inngestFailureErrorSchema,
      event: eventSchema,
    }),
  });
}

export const inngestFunctionCancelledEnvelopeSchema = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("inngest/function.cancelled"),
  data: Schema.Struct({
    function_id: inngestEventIdentitySchema,
    run_id: inngestEventIdentitySchema,
    correlation_id: Schema.optionalKey(Schema.String),
    /** The cancelled run's triggering event; each handler decodes what it needs from it. */
    event: Schema.optionalKey(Schema.Unknown),
  }),
});

export const inngestFunctionFinishedEnvelopeSchema = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("inngest/function.finished"),
  data: Schema.Struct({
    function_id: inngestEventIdentitySchema,
    run_id: inngestEventIdentitySchema,
    error: Schema.optionalKey(Schema.NullOr(inngestFailureErrorSchema)),
    event: Schema.optionalKey(Schema.Unknown),
  }),
});

export const inngestFunctionFinishedEventType = eventType(
  "inngest/function.finished",
  { schema: staticSchema<{ function_id: string; run_id: string; error?: { name: string; message: string } | null }>() },
);

export const inngestFunctionCancelledEventType = eventType(
  "inngest/function.cancelled",
  {
    schema: staticSchema<{ function_id: string; run_id: string; correlation_id?: string; event?: unknown }>(),
  },
);
