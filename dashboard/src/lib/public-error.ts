import { Schema } from "effect";

export const PublicErrorCategory = Schema.Literals([
  "validation",
  "unauthorized",
  "forbidden",
  "not-found",
  "conflict",
  "rate-limited",
  "build-grant-unavailable",
  "unavailable",
  "server-lacks",
  "internal",
]);

export type PublicErrorCategory = typeof PublicErrorCategory.Type;

/** The wire code each category answers with. */
export const PUBLIC_ERROR_CODES = {
  validation: "VALIDATION_FAILED",
  unauthorized: "UNAUTHORIZED",
  forbidden: "FORBIDDEN",
  "not-found": "NOT_FOUND",
  conflict: "CONFLICT",
  "rate-limited": "RATE_LIMITED",
  "build-grant-unavailable": "BUILD_GRANT_UNAVAILABLE",
  unavailable: "UNAVAILABLE",
  "server-lacks": "SERVER_LACKS",
  internal: "INTERNAL",
} as const satisfies Record<PublicErrorCategory, string>;

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
