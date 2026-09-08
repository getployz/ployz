import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import type {
  ClusterTeardown,
  DeployOutcome,
  ExecutionError,
  MachineId,
} from "@ployz/sdk";
import { Effect, Option } from "effect";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { PloyzStepTools } from "#/modules/inngest/client";
import * as activities from "#/modules/runtime/teardown-activities.server";
import type { TeardownAttempt } from "#/modules/runtime/teardown.repository";
import {
  createCancelTeardown,
  createProcessTeardown,
  decodeTeardownFailureEvent,
  executeProcessTeardown,
  executeProcessTeardownOnFailure,
} from "#/modules/runtime/teardown.inngest";

const activity = {
  prepare: vi.fn(),
  destroyCluster: vi.fn(),
  destroyEnvironment: vi.fn(),
  revokePairing: vi.fn(),
  recordRuntimeEvidence: vi.fn(),
  dropCloudRows: vi.fn(),
  complete: vi.fn(),
  failOwned: vi.fn(),
};

vi.spyOn(activities, "prepareTeardownAttemptActivity").mockImplementation(
  (input) => Effect.promise(() => activity.prepare(input)),
);
vi.spyOn(activities, "destroyClusterActivity").mockImplementation((input) =>
  Effect.promise(() => activity.destroyCluster(input)));
vi.spyOn(activities, "destroyEnvironmentActivity").mockImplementation((input) =>
  Effect.promise(() => activity.destroyEnvironment(input)));
vi.spyOn(activities, "revokeTeardownPairingActivity").mockImplementation(
  (input) => Effect.promise(() => activity.revokePairing(input)),
);
vi.spyOn(activities, "recordTeardownRuntimeEvidenceActivity").mockImplementation(
  (input) => Effect.promise(() => activity.recordRuntimeEvidence(input)),
);
vi.spyOn(activities, "dropTeardownCloudRowsActivity").mockImplementation((input) =>
  Effect.promise(() => activity.dropCloudRows(input)));
vi.spyOn(activities, "completeTeardownAttemptActivity").mockImplementation(
  (input) => Effect.promise(() => activity.complete(input)),
);
vi.spyOn(activities, "failOwnedTeardownAttemptActivity").mockImplementation(
  (input) => Effect.promise(() => activity.failOwned(input)),
);

function createStepTools(): Pick<PloyzStepTools, "run"> {
  const run: PloyzStepTools["run"] = async (_id, operation, ...input) => {
    const value = await operation(...input);
    return value === undefined ? null : JSON.parse(JSON.stringify(value));
  };
  return { run };
}

function serializedAttempt(input: {
  scope: "environment" | "organization";
  runtimeMembership: "verified" | "unknown" | "untouched";
  destroyRuntimeProjects: boolean;
  environments?: Array<{
    environmentId: string;
    projectId: string;
    projectName: string;
    cloudName: string;
  }>;
}) {
  return asTestDouble<TeardownAttempt>()({
    id: "attempt-1",
    organizationId: "organization-1",
    requestedByUserId: "user-1",
    projectId: input.scope === "organization" ? null : "project-1",
    environmentId: input.scope === "organization" ? null : "environment-1",
    scope: input.scope,
    confirmDataLoss: [],
    targets: {
      environments: input.environments ?? [],
      destroyRuntimeProjects: input.destroyRuntimeProjects,
      revokePairing: input.runtimeMembership !== "untouched",
      runtimeMembership: input.runtimeMembership,
    },
    status: "running",
    inngestRunId: "run-1",
    outcome: null,
    failureMessage: null,
    createdAt: "2026-09-08T00:00:00.000Z",
    startedAt: "2026-09-08T00:00:00.000Z",
    terminalAt: null,
    updatedAt: "2026-09-08T00:00:00.000Z",
  });
}

async function execute(attempt: TeardownAttempt) {
  activity.prepare.mockResolvedValue({ kind: "ready", attempt });
  return executeProcessTeardown({
    event: { data: { attemptId: attempt.id } },
    step: createStepTools(),
    runId: "run-1",
  });
}

describe("teardown Inngest boundary", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    activity.complete.mockImplementation(async (input) => ({
      id: input.attemptId,
      status: input.status,
    }));
    activity.dropCloudRows.mockResolvedValue(undefined);
    activity.recordRuntimeEvidence.mockResolvedValue(undefined);
    activity.revokePairing.mockResolvedValue({ rustMustRevokePairing: false });
    activity.failOwned.mockResolvedValue({ state: "partial" });
  });

  it("does not automatically replay teardown mutations", () => {
    expect(createProcessTeardown(new Inngest({ id: "test" })).opts).toEqual(
      expect.objectContaining({ retries: 0 }),
    );
  });

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

  it("persists an incomplete ClusterTeardown before Cloud cleanup", async () => {
    const clusterTeardown = {
      destroyed_projects: [],
      machines: {
        successes: [],
        failures: [
          {
            machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
            error: {
              code: "unavailable",
              message: "machine did not answer",
              details: null,
            },
          },
        ],
        omissions: [],
      },
      pairing_revoked: true,
    } satisfies ClusterTeardown;
    activity.destroyCluster.mockResolvedValue(clusterTeardown);

    const result = await execute(
      serializedAttempt({
        scope: "organization",
        runtimeMembership: "verified",
        destroyRuntimeProjects: false,
      }),
    );

    expect(result).toEqual({ attemptId: "attempt-1", status: "partial" });
    expect(activity.destroyEnvironment).not.toHaveBeenCalled();
    expect(activity.dropCloudRows).not.toHaveBeenCalled();
    expect(activity.recordRuntimeEvidence).toHaveBeenCalledWith(
      expect.objectContaining({
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "unknown",
          clusterTeardown,
        },
      }),
    );
    expect(activity.complete).toHaveBeenCalledWith(
      expect.objectContaining({
        status: "partial",
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "unknown",
          clusterTeardown,
        },
      }),
    );
  });

  it("records a complete ClusterTeardown before Cloud cleanup", async () => {
    const clusterTeardown = {
      destroyed_projects: ["app-production"],
      machines: {
        successes: [
          {
            machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
            value: { reset_warning: "replicated delete failed" },
          },
        ],
        failures: [],
        omissions: [],
      },
      pairing_revoked: true,
    } satisfies ClusterTeardown;
    activity.destroyCluster.mockResolvedValue(clusterTeardown);

    const result = await execute(
      serializedAttempt({
        scope: "organization",
        runtimeMembership: "verified",
        destroyRuntimeProjects: false,
      }),
    );

    expect(result).toEqual({ attemptId: "attempt-1", status: "completed" });
    expect(activity.recordRuntimeEvidence).toHaveBeenCalledWith(
      expect.objectContaining({
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "verified_zero",
          clusterTeardown,
        },
      }),
    );
    const recordOrder = activity.recordRuntimeEvidence.mock.invocationCallOrder.at(0);
    const dropOrder = activity.dropCloudRows.mock.invocationCallOrder.at(0);
    if (recordOrder === undefined || dropOrder === undefined) {
      throw new Error("Expected runtime evidence and Cloud cleanup steps.");
    }
    expect(recordOrder).toBeLessThan(dropOrder);
  });

  it("does not clean Cloud rows when Cluster pairing revocation is incomplete", async () => {
    const clusterTeardown = {
      destroyed_projects: [],
      machines: { successes: [], failures: [], omissions: [] },
      pairing_revoked: false,
    } satisfies ClusterTeardown;
    activity.destroyCluster.mockResolvedValue(clusterTeardown);

    const result = await execute(
      serializedAttempt({
        scope: "organization",
        runtimeMembership: "verified",
        destroyRuntimeProjects: false,
      }),
    );

    expect(result).toEqual({ attemptId: "attempt-1", status: "partial" });
    expect(activity.dropCloudRows).not.toHaveBeenCalled();
    expect(activity.complete).toHaveBeenCalledWith(
      expect.objectContaining({
        status: "partial",
        outcome: {
          rustMustRevokePairing: true,
          runtimeMembership: "unknown",
          clusterTeardown,
        },
      }),
    );
  });

  it("persists a failed project DeployOutcome before Cloud cleanup", async () => {
    const projectOutcome = {
      type: "failed",
      completed: [],
      failed: {
        type: "operation",
        operation: {
          type: "remove_volume",
          id: {
            machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
            name: "data",
          },
        },
        error: { type: "cancelled" },
      },
      unexecuted: [],
    } satisfies DeployOutcome<ExecutionError>;
    activity.destroyEnvironment.mockResolvedValue(projectOutcome);

    const result = await execute(
      serializedAttempt({
        scope: "environment",
        runtimeMembership: "untouched",
        destroyRuntimeProjects: true,
        environments: [
          {
            environmentId: "environment-1",
            projectId: "project-1",
            projectName: "app-production",
            cloudName: "acme/app/Production",
          },
        ],
      }),
    );

    expect(result).toEqual({ attemptId: "attempt-1", status: "partial" });
    expect(activity.dropCloudRows).not.toHaveBeenCalled();
    expect(activity.recordRuntimeEvidence).toHaveBeenCalledWith(
      expect.objectContaining({
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "untouched",
          projectTeardowns: [{ projectName: "app-production", outcome: projectOutcome }],
        },
      }),
    );
    expect(activity.complete).toHaveBeenCalledWith(
      expect.objectContaining({
        status: "partial",
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "untouched",
          projectTeardowns: [{ projectName: "app-production", outcome: projectOutcome }],
        },
      }),
    );
  });

  it("keeps a completed project prefix before a later Runtime call is unknown", async () => {
    const firstOutcome = {
      type: "success",
      completed: [],
    } satisfies DeployOutcome<ExecutionError>;
    activity.destroyEnvironment
      .mockResolvedValueOnce(firstOutcome)
      .mockRejectedValueOnce(new Error("second project connection ended"));
    const attempt = serializedAttempt({
      scope: "environment",
      runtimeMembership: "untouched",
      destroyRuntimeProjects: true,
      environments: [
        {
          environmentId: "environment-1",
          projectId: "project-1",
          projectName: "app-production",
          cloudName: "acme/app/Production",
        },
        {
          environmentId: "environment-2",
          projectId: "project-1",
          projectName: "app-staging",
          cloudName: "acme/app/Staging",
        },
      ],
    });

    await expect(execute(attempt)).rejects.toThrow("Effect activity failed.");

    expect(activity.recordRuntimeEvidence).toHaveBeenCalledWith(
      expect.objectContaining({
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "untouched",
          projectTeardowns: [
            { projectName: "app-production", outcome: firstOutcome },
          ],
        },
      }),
    );
    expect(activity.dropCloudRows).not.toHaveBeenCalled();
  });

  it("skips Runtime projects that lacked a Cloud connection at confirmation", async () => {
    const attempt = serializedAttempt({
      scope: "environment",
      runtimeMembership: "untouched",
      destroyRuntimeProjects: false,
      environments: [
        {
          environmentId: "environment-1",
          projectId: "project-1",
          projectName: "app-production",
          cloudName: "acme/app/Production",
        },
      ],
    });

    const result = await execute(attempt);

    expect(result).toEqual({ attemptId: "attempt-1", status: "completed" });
    expect(activity.destroyEnvironment).not.toHaveBeenCalled();
    expect(activity.recordRuntimeEvidence).toHaveBeenCalledWith(
      expect.objectContaining({
        outcome: {
          rustMustRevokePairing: false,
          runtimeMembership: "untouched",
          projectTeardowns: [],
        },
      }),
    );
    const recordOrder = activity.recordRuntimeEvidence.mock.invocationCallOrder.at(0);
    const dropOrder = activity.dropCloudRows.mock.invocationCallOrder.at(0);
    if (recordOrder === undefined || dropOrder === undefined) {
      throw new Error("Expected runtime evidence and Cloud cleanup steps.");
    }
    expect(recordOrder).toBeLessThan(dropOrder);
  });

  it("terminalizes an active teardown through the failure handler", async () => {
    await executeProcessTeardownOnFailure({
      event: {
        name: "inngest/function.failed",
        data: {
          function_id: "process-teardown",
          run_id: "run-1",
          error: { name: "Error", message: "connection ended" },
          event: {
            name: "cloud/teardown.requested",
            data: { attemptId: "attempt-1" },
          },
        },
      },
    });

    expect(activity.failOwned).toHaveBeenCalledWith({
      attemptId: "attempt-1",
      inngestRunId: "run-1",
      failureMessage: "connection ended",
      now: expect.any(Date),
    });
  });
});
