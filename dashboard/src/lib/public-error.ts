import { Schema } from "effect";

export const PublicError = Schema.Union([
  Schema.TaggedStruct("PublicError", {
    code: Schema.Literals([
      "VALIDATION_FAILED",
      "UNAUTHORIZED",
      "FORBIDDEN",
      "NOT_FOUND",
      "CONFLICT",
      "RATE_LIMITED",
      "BUILD_GRANT_UNAVAILABLE",
      "UNAVAILABLE",
      "INTERNAL",
    ]),
    message: Schema.String,
  }),
  Schema.TaggedStruct("PublicError", {
    code: Schema.Literal("SERVER_LACKS"),
    message: Schema.String,
    machineId: Schema.String,
    capability: Schema.String,
  }),
]);

export type PublicError = typeof PublicError.Type;
