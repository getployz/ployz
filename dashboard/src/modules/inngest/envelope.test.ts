import { Schema } from "effect";
import { NonRetriableError } from "inngest";
import { describe, expect, it } from "vitest";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";

const Envelope = Schema.Struct({
  name: Schema.Literal("example/event"),
  data: Schema.Struct({ id: Schema.String.check(Schema.isNonEmpty()) }),
});

describe("decodeInngestEnvelope", () => {
  it("returns the decoded envelope", () => {
    expect(
      decodeInngestEnvelope(Envelope)({
        name: "example/event",
        data: { id: "event-1" },
      }),
    ).toEqual({
      name: "example/event",
      data: { id: "event-1" },
    });
  });

  it("throws NonRetriableError for an invalid envelope", () => {
    expect(() => decodeInngestEnvelope(Envelope)({ name: "example/event" })).toThrow(
      NonRetriableError,
    );
    try {
      decodeInngestEnvelope(Envelope)({ name: "other/event", data: { id: "event-1" } });
    } catch (cause) {
      expect(cause).toBeInstanceOf(NonRetriableError);
      expect(cause).toMatchObject({
        message: "Inngest event envelope is invalid.",
      });
    }
  });
});
