import { describe, expect, it } from "vitest";
import { hasPublicErrorCode, PublicError } from "#/lib/public-error";

describe("public error wire contract", () => {
  it("recognizes only strict encoded public errors with the requested code", () => {
    const error: PublicError = {
      _tag: "PublicError",
      code: "NOT_FOUND",
      message: "The requested resource was not found.",
    };

    expect(hasPublicErrorCode(error, "NOT_FOUND")).toBe(true);
    expect(hasPublicErrorCode(error, "INTERNAL")).toBe(false);
    expect(
      hasPublicErrorCode({ ...error, internal: "secret" }, "NOT_FOUND"),
    ).toBe(false);
    expect(hasPublicErrorCode(new Error("NOT_FOUND"), "NOT_FOUND")).toBe(
      false,
    );
  });
});
