import { Schema } from "effect";

export const PublicError = Schema.Union([
  Schema.TaggedStruct("PublicError", {
    code: Schema.Literals([
      "VALIDATION_FAILED",
      "UNAUTHORIZED",
      "FORBIDDEN",
      "NOT_FOUND",
      "CONFLICT",
      "BUILD_GRANT_UNAVAILABLE",
      "INTERNAL",
    ]),
    message: Schema.String,
  }),
  /** The Server `machineId` lacks `capability`, a name from its daemon's refusal. */
  Schema.TaggedStruct("PublicError", {
    code: Schema.Literal("SERVER_LACKS"),
    message: Schema.String,
    machineId: Schema.String,
    capability: Schema.String,
  }),
]);

export type PublicError = typeof PublicError.Type;
