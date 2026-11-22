import { Option, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { inngestEventEnvelopeFields, inngestFunctionFailedEnvelopeSchema } from "#/modules/inngest/events";

describe("Inngest events", () => {
  it("decodes complete function-failed envelopes for an owned event", () => {
    const ownedEvent = Schema.Struct({
      ...inngestEventEnvelopeFields,
      name: Schema.Literal("owned/requested"),
      data: Schema.Struct({ attemptId: Schema.String }),
    });
    const decode = Schema.decodeUnknownOption(
      inngestFunctionFailedEnvelopeSchema(ownedEvent),
    );
    const valid = decode({
      id: "failed-event-1",
      ts: 1,
      name: "inngest/function.failed",
      data: {
        function_id: "owned-function",
        run_id: "run-1",
        error: { name: "Error", message: "failed" },
        event: {
          name: "owned/requested",
          data: { attemptId: "attempt-1" },
        },
      },
    });

    expect(Option.isSome(valid)).toBe(true);
    expect(
      Option.isNone(
        decode({
          name: "inngest/function.failed",
          data: {
            function_id: " ",
            run_id: "run-1",
            error: { name: "Error", message: "failed" },
            event: {
              name: "owned/requested",
              data: { attemptId: "attempt-1" },
            },
          },
        }),
      ),
    ).toBe(true);
  });
});
