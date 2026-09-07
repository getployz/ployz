import { InngestTestEngine } from "@inngest/test";
import { Effect, Option } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DestructiveVolumeAttemptRecord } from "#/modules/operations/destructive-volume-attempt.repository";
import * as attemptRepository from "#/modules/operations/destructive-volume-attempt.repository";
import * as dispatch from "#/modules/operations/destructive-volume-dispatch.server";
import * as workflow from "#/modules/operations/destructive-volume-workflow.server";
import { createCancelDestructiveVolume } from "./cancellation";
import {
  createProcessDestructiveVolume,
  decodeDestructiveVolumeFailureEvent,
  PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID,
} from "./processor";
import { createRecoverDestructiveVolumeOutbox } from "./scheduled-recovery";

vi.spyOn(
  workflow,
  "loadDestructiveVolumeWorkflowContext",
).mockImplementation(({ attemptId }) =>
  Effect.succeed({
    organizationId: "organization-1",
    attempt: { state: "terminal", id: attemptId, disposition: "completed" },
  }),
);
vi.spyOn(
  workflow,
  "loadDestructiveVolumeCancellationContext",
).mockImplementation(({ inngestRunId }) =>
  Effect.succeed({
    organizationId: "organization-1",
    attempt: {
      state: "owned_unassociated",
      id: "attempt-1",
      target: reviewedTarget,
      evidence: reviewedEvidence,
      inngestRunId,
      deadlineAt: new Date("2026-07-10T12:00:00.000Z"),
    },
  }),
);
vi.spyOn(
  attemptRepository,
  "finalizeUnassociatedDestructiveVolumeAttempt",
).mockImplementation(() =>
  Effect.succeed({ state: "recorded", attempt: attemptRecord() }),
);
vi.spyOn(
  attemptRepository,
  "listUnpublishedDestructiveVolumeAttempts",
).mockImplementation(() => Effect.succeed([]));
vi.spyOn(dispatch, "dispatchDestructiveVolumeAttempt").mockImplementation(() =>
  Effect.succeed({ acknowledged: true }),
);

describe("destructive volume Inngest adapters", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("executes the processor against a durable terminal attempt", async () => {
    const output = await new InngestTestEngine({
      function: createProcessDestructiveVolume(new Inngest({ id: "test" })),
      events: [
        {
          name: "cloud/destructive-volume.requested",
          data: { attemptId: "attempt-1" },
        },
      ],
    }).execute();

    expect(output.result).toEqual({
      attemptId: "attempt-1",
      status: "completed",
      skipped: true,
    });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "load-destructive-volume-context",
      expect.any(Function),
    );
  });

  it("rejects an invalid processor envelope inside the first step", async () => {
    const output = await new InngestTestEngine({
      function: createProcessDestructiveVolume(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "cloud/destructive-volume.requested",
          data: { attemptId: "" },
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

  it("decodes a complete failure envelope before reading its durable owner", () => {
    const decoded = decodeDestructiveVolumeFailureEvent({
      name: "inngest/function.failed",
      data: {
        function_id: PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID,
        run_id: "run-1",
        error: { name: "Error", message: "failed" },
        event: {
          name: "cloud/destructive-volume.requested",
          data: { attemptId: "attempt-1" },
        },
      },
    });
    expect(Option.isSome(decoded)).toBe(true);
    expect(
      Option.isNone(
        decodeDestructiveVolumeFailureEvent({
          name: "inngest/function.failed",
          data: {
            function_id: PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID,
            run_id: "run-1",
            error: { name: "Error", message: "failed" },
            event: {
              name: "cloud/destructive-volume.requested",
              data: { attemptId: "" },
            },
          },
        }),
      ),
    ).toBe(true);
  });

  it("loads cancellation authority inside a durable step before terminalizing", async () => {
    const output = await new InngestTestEngine({
      function: createCancelDestructiveVolume(new Inngest({ id: "test" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID,
            run_id: "run-1",
          },
        },
      ],
    }).execute();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({
      state: "recorded",
      attempt: expect.objectContaining({
        id: "attempt-1",
        disposition: "cloud_cancelled",
      }),
    });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "load-destructive-volume-cancellation-context",
      expect.any(Function),
    );
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "cancel-unassociated-destructive-volume",
      expect.any(Function),
    );
  });

  it("rejects an invalid cancellation envelope before persistence", async () => {
    const output = await new InngestTestEngine({
      function: createCancelDestructiveVolume(new Inngest({ id: "test" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID },
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

  it("executes scheduled outbox recovery through its durable listing step", async () => {
    const output = await new InngestTestEngine({
      function: createRecoverDestructiveVolumeOutbox(
        new Inngest({ id: "test" }),
      ),
    }).execute();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ published: 0 });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "list-released-destructive-volume-attempts",
      expect.any(Function),
    );
  });
});

const reviewedTarget = {
  version: 1 as const,
  resourceId: "resource-1",
  namespaceId: "namespace-1",
  volumeName: "volume-1",
  machineId: "machine-1",
};

const reviewedEvidence = {
  version: 1 as const,
  fingerprint: "fingerprint-1",
  reviewedAt: "2026-07-10T11:00:00.000Z",
  evidence: {
    namespaceId: "namespace-1",
    volumeName: "volume-1",
    machineId: "machine-1",
    kind: { kind: "plain" as const },
    availability: { status: "no_answer" as const },
    referencingServices: [],
  },
};

function attemptRecord(): DestructiveVolumeAttemptRecord & {
  organizationId: string;
} {
  return {
    id: "attempt-1",
    organizationId: "organization-1",
    environmentDeploymentId: "deployment-1",
    environmentResourceId: "resource-1",
    retryOfAttemptId: null,
    target: reviewedTarget,
    evidence: reviewedEvidence,
    evidenceFingerprint: "fingerprint-1",
    disposition: "cloud_cancelled",
    operationId: null,
    startSequence: null,
    inngestRunId: "run-1",
    requestPublishedAt: null,
    acceptedAt: null,
    terminalEvent: null,
    failure: null,
    deadlineAt: new Date("2026-07-10T12:00:00.000Z"),
    terminalAt: new Date("2026-07-10T11:30:00.000Z"),
    createdAt: new Date("2026-07-10T11:00:00.000Z"),
    updatedAt: new Date("2026-07-10T11:30:00.000Z"),
  };
}
