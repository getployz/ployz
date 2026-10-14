import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { Option } from "effect";
import { describe, expect, it } from "vitest";
import {
  createCancelMachineRemove,
  createProcessMachineRemove,
  decodeMachineRemoveFailureEvent,
  isLastTry,
  MACHINE_REMOVE_RETRIES,
} from "#/modules/machines/machine-removal.inngest";

describe("machine remove Inngest boundary", () => {
  it("rejects malformed request events before an activity can run", async () => {
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [
        {
          name: "machine/remove.requested",
          data: { attemptId: "", extra: "forward-compatible" },
        },
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
      function: createCancelMachineRemove(new Inngest({ id: "ployz-cloud" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: "ployz-cloud-process-environment-deployment",
            run_id: "run-1",
          },
        },
      ],
    }).execute();

    expect(output.result).toEqual({ skipped: true });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "decode-machine-remove-cancellation-event",
      expect.any(Function),
    );
  });

  it("cancels the attempt when Inngest cancels the app's process-machine-remove", async () => {
    const output = await new InngestTestEngine({
      function: createCancelMachineRemove(new Inngest({ id: "ployz-cloud" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: "ployz-cloud-process-machine-remove", run_id: "run-1" },
        },
      ],
      steps: [{ id: "cancel-machine-remove", handler: () => ({ cancelled: true }) }],
    }).execute();

    expect(output.result).toEqual({ cancelled: true });
  });

  it("rejects malformed cancellation envelopes in the owned step", async () => {
    const output = await new InngestTestEngine({
      function: createCancelMachineRemove(new Inngest({ id: "test" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: "process-machine-remove" },
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

  it("decodes the complete failure envelope before exposing durable identity", () => {
    const decoded = decodeMachineRemoveFailureEvent({
      name: "inngest/function.failed",
      data: {
        function_id: "process-machine-remove",
        run_id: "run-1",
        error: { name: "Error", message: "failed" },
        event: {
          name: "machine/remove.requested",
          data: { attemptId: "attempt-1" },
        },
      },
    });

    expect(Option.isSome(decoded)).toBe(true);
    expect(
      Option.isNone(
        decodeMachineRemoveFailureEvent({
          name: "inngest/function.failed",
          data: {
            function_id: "process-machine-remove",
            run_id: "run-1",
            error: { name: "Error", message: "failed" },
            event: { name: "wrong/event", data: { attemptId: "attempt-1" } },
          },
        }),
      ),
    ).toBe(true);
  });

  it("fails the attempt, and releases nothing, when the Server refuses its removal", async () => {
    const attemptId = "00000000-0000-4000-8000-000000000a01";
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [{ name: "machine/remove.requested", data: { attemptId } }],
      steps: [
        { id: "claim-machine-remove", handler: () => ({ kind: "ready", attempt: { id: attemptId } }) },
        { id: "observe-leaving-server", handler: () => ({ machine: { id: "machine-1", name: "two" }, writes: ["data"] }) },
        { id: "remove-machine", handler: () => ({ kind: "refused", failureCode: "conflict", message: "Server two answers and holds copies of Volumes data; removing it without a reset leaves them behind. No changes made." }) },
        { id: "complete-refused", handler: () => undefined },
      ],
    }).execute();

    expect(output.result).toEqual({ attemptId, status: "failed" });
    expect(output.ctx.step.run).toHaveBeenCalledWith("complete-refused", expect.any(Function));
    expect(output.ctx.step.run).not.toHaveBeenCalledWith("release-server", expect.any(Function));
    expect(output.ctx.step.run).not.toHaveBeenCalledWith("release-volume-lanes", expect.any(Function));
  });

  it("leaves a superseded attempt alone: nothing is released, completed or synced", async () => {
    const attemptId = "00000000-0000-4000-8000-000000000a02";
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [{ name: "machine/remove.requested", data: { attemptId } }],
      steps: [
        { id: "claim-machine-remove", handler: () => ({ kind: "ready", attempt: { id: attemptId } }) },
        { id: "observe-leaving-server", handler: () => ({ machine: { id: "machine-1", name: "two" }, writes: ["data"] }) },
        { id: "remove-machine", handler: () => ({ kind: "superseded" }) },
      ],
    }).execute();

    expect(output.result).toEqual({ attemptId, status: "superseded", skipped: true });
    const steps = output.ctx.step.run.mock.calls.map(([id]) => id);
    expect(steps).toEqual(["normalize-machine-remove-attempt-id", "claim-machine-remove", "observe-leaving-server", "remove-machine"]);
    expect(output.ctx.step.sendEvent).not.toHaveBeenCalled();
  });

  it("finishes a Server that reset before its Forget with Forget alone, then releases it", async () => {
    const attemptId = "00000000-0000-4000-8000-000000000a03";
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [{ name: "machine/remove.requested", data: { attemptId } }],
      steps: [
        { id: "claim-machine-remove", handler: () => ({ kind: "ready", attempt: { id: attemptId, organizationId: "org-1", machineId: "machine-1", noReset: false } }) },
        { id: "observe-leaving-server", handler: () => ({ machine: { id: "machine-1", name: "two" }, writes: ["data"] }) },
        { id: "remove-machine", handler: () => ({ kind: "left", resetWarning: "Docker reset failed", generation: "1" }) },
        { id: "forget-machine", handler: () => ({ kind: "removed", resetWarning: "Docker reset failed", generation: "1" }) },
        { id: "release-volume-lanes", handler: () => [] },
        { id: "release-server", handler: () => ({ kind: "kept", reason: "its reset didn't finish" }) },
        { id: "complete-succeeded", handler: () => undefined },
        { id: "request-cluster-domain-sync", handler: () => undefined },
      ],
    }).execute();

    expect(output.result).toEqual({ attemptId, status: "succeeded" });
    const steps = output.ctx.step.run.mock.calls.map(([id]) => id);
    expect(steps).toEqual([
      "normalize-machine-remove-attempt-id",
      "claim-machine-remove",
      "observe-leaving-server",
      "remove-machine",
      "forget-machine",
      "release-volume-lanes",
      "release-server",
      "complete-succeeded",
    ]);
  });

  it("observes the leaving Server as its last try only on the step's final attempt", () => {
    expect(isLastTry(0)).toBe(false);
    expect(isLastTry(MACHINE_REMOVE_RETRIES - 1)).toBe(false);
    expect(isLastTry(MACHINE_REMOVE_RETRIES)).toBe(true);
  });
});
