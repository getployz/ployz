import { Schema } from "effect";

export const PublicError = Schema.TaggedStruct("PublicError", {
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
});

export type PublicError = typeof PublicError.Type;
