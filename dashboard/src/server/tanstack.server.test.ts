import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { strictValidator } from "#/server/tanstack";

const Input = Schema.Struct({
  id: Schema.String,
});

describe("strictValidator", () => {
  it("accepts a payload that matches the schema", () => {
    expect(
      strictValidator(Input)["~standard"].validate({ id: "row-1" }),
    ).toEqual({ value: { id: "row-1" } });
  });

  it("rejects excess properties", () => {
    expect(
      strictValidator(Input)["~standard"].validate({
        id: "row-1",
        extra: true,
      }),
    ).toMatchObject({ issues: expect.any(Array) });
  });
});
