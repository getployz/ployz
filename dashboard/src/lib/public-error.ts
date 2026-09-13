import { Option, Schema } from "effect";

export const PublicError = Schema.TaggedStruct("PublicError", {
  code: Schema.Literals([
    "VALIDATION_FAILED",
    "UNAUTHORIZED",
    "FORBIDDEN",
    "NOT_FOUND",
    "CONFLICT",
    "INTERNAL",
  ]),
  message: Schema.String,
});

export type PublicError = typeof PublicError.Type;
export type PublicErrorCode = PublicError["code"];

const decodePublicError = Schema.decodeUnknownOption(PublicError);

export function hasPublicErrorCode(
  cause: unknown,
  code: PublicErrorCode,
): boolean {
  const decoded = decodePublicError(cause, { onExcessProperty: "error" });
  return Option.isSome(decoded) && decoded.value.code === code;
}
