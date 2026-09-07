import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { inngestFunctionCancelledEventType } from "#/modules/inngest/events";
import * as runtimeCancellation from "#/modules/deployments/runtime-cancellation.repository.server";
import { createMarkCancelledRowBackedWorkflow } from "./environment-deployment.inngest";

const cancelDeployment = vi.fn();

vi.spyOn(
  runtimeCancellation,
  "markCancelledByInngestRunId",
).mockImplementation((runId) =>
  Effect.promise(() => cancelDeployment(runId)),
);

describe("row-backed cancellation Inngest adapter", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    cancelDeployment.mockResolvedValue(true);
  });

  it("preserves the cancellation trigger and terminalizes by original run id", async () => {
    const markCancelledRowBackedWorkflow = createMarkCancelledRowBackedWorkflow(
      new Inngest({ id: "test" }),
    );
    expect(markCancelledRowBackedWorkflow.opts).toEqual(
      expect.objectContaining({
        id: "mark-cancelled-row-backed-workflow",
        triggers: [{ event: inngestFunctionCancelledEventType }],
      }),
    );
    const output = await new InngestTestEngine({
      function: markCancelledRowBackedWorkflow,
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: "process-environment-deployment",
            run_id: "deployment-run-1",
          },
        },
      ],
    }).execute();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({
      functionId: "process-environment-deployment",
      runId: "deployment-run-1",
      marked: true,
    });
    expect(cancelDeployment).toHaveBeenCalledTimes(1);
    expect(cancelDeployment).toHaveBeenCalledWith("deployment-run-1");
  });

  it("rejects an incomplete cancellation envelope before persistence", async () => {
    const output = await new InngestTestEngine({
      function: createMarkCancelledRowBackedWorkflow(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: "process-environment-deployment" },
        },
      ],
    }).execute();

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Inngest event envelope is invalid.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
    expect(cancelDeployment).not.toHaveBeenCalled();
  });
});
