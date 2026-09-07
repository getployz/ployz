import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { Option } from "effect";
import { describe, expect, it } from "vitest";
import {
  createCancelTeardown,
  createProcessTeardown,
  decodeTeardownFailureEvent,
} from "#/modules/runtime/teardown.inngest";

describe("teardown Inngest boundary", () => {
  it("rejects malformed request events before an activity can run", async () => {
    const output = await new InngestTestEngine({
      function: createProcessTeardown(new Inngest({ id: "test" })),
      events: [
        { name: "cloud/teardown.requested", data: { attemptId: "" } },
      ],
    }).execute();
    expect(output.result).toEqual({
      attemptId: null,
      status: "invalid",
      skipped: true,
    });
    expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
  });

  it("ignores cancellation events owned by another function", async () => {
    const output = await new InngestTestEngine({
      function: createCancelTeardown(new Inngest({ id: "test" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: "process-environment-deployment",
            run_id: "run-1",
          },
        },
      ],
    }).execute();
    expect(output.result).toEqual({ skipped: true });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "decode-teardown-cancellation-event",
      expect.any(Function),
    );
  });

  it("rejects malformed cancellation envelopes in the owned step", async () => {
    const output = await new InngestTestEngine({
      function: createCancelTeardown(new Inngest({ id: "test" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: "process-teardown" },
        },
      ],
    }).execute();
    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Inngest event envelope is invalid.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
    expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
  });

  it("decodes the complete failure envelope before reading its message", () => {
    const decoded = decodeTeardownFailureEvent({
      name: "inngest/function.failed",
      data: {
        function_id: "process-teardown",
        run_id: "run-1",
        error: { name: "Error", message: "failed" },
        event: {
          name: "cloud/teardown.requested",
          data: { attemptId: "attempt-1" },
        },
      },
    });
    expect(Option.isSome(decoded)).toBe(true);
    expect(
      Option.isNone(
        decodeTeardownFailureEvent({
          name: "inngest/function.failed",
          data: {
            function_id: "process-teardown",
            run_id: "run-1",
            error: { name: "Error" },
            event: {
              name: "cloud/teardown.requested",
              data: { attemptId: "attempt-1" },
            },
          },
        }),
      ),
    ).toBe(true);
  });
});
