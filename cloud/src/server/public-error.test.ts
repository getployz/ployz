import { Cause, Data } from "effect";
import { describe, expect, it, vi } from "vitest";
import {
  encodePublicBoundaryError,
  encodePublicError,
  NotFound,
  publicErrorResponse,
} from "#/server/public-error";

class DatabaseFailure extends Data.TaggedError("DatabaseFailure")<{
  readonly cause: Error;
  readonly providerBody: string;
  readonly secret: string;
}> {}

class ValidationFailure extends Data.TaggedError("Validation")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "validation" as const;
}

describe("encodePublicError", () => {
  it("maps capability not-found tags", () => {
    expect(
      encodePublicError(
        new NotFound({ message: "secret" }),
      ),
    ).toEqual({
      _tag: "PublicError",
      code: "NOT_FOUND",
      message: "The requested resource was not found.",
    });
  });

  it("encodes infrastructure failures as one plain redacted DTO", () => {
    const encoded = encodePublicError(
      new DatabaseFailure({
        cause: new Error("database host db.internal.test"),
        providerBody: "provider-token",
        secret: "application-secret",
      }),
    );

    expect(encoded).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
    expect(encoded).not.toBeInstanceOf(Error);
    expect(JSON.stringify(encoded)).not.toMatch(
      /application-secret|provider-token|db\.internal|stack|cause/u,
    );
  });

  it("maps capability validation failures without exposing their details", () => {
    expect(
      encodePublicError(
        new ValidationFailure({ message: "secret-bearing validation context" }),
      ),
    ).toEqual({
      _tag: "PublicError",
      code: "VALIDATION_FAILED",
      message: "The request is invalid.",
    });
  });

  it("does not report expected typed failures", () => {
    const report = vi.fn();

    publicErrorResponse(
      new DatabaseFailure({
        cause: new Error("offline"),
        providerBody: "provider-token",
        secret: "application-secret",
      }),
      { report },
    );

    expect(report).not.toHaveBeenCalled();
  });

  it("reports full defect and interruption causes separately while redacting transport", async () => {
    const report = vi.fn();
    const defect = Cause.die(new Error("provider-token"));
    const defectError = encodePublicBoundaryError(defect, { report });
    const interruption = Cause.interrupt(7);
    const interruptionError = encodePublicBoundaryError(interruption, { report });

    expect(report).toHaveBeenNthCalledWith(1, "defect", defect);
    expect(report).toHaveBeenNthCalledWith(2, "interruption", interruption);
    expect(defectError).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
    expect(interruptionError).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
  });
});
