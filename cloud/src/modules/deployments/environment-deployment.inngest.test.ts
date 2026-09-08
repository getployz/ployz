import { beforeEach, describe, expect, it, vi } from "vitest";
import { Effect } from "effect";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient } from "#/modules/inngest/client";
import { NonRetriableError, serializeError, StepError } from "inngest";
import {
  DEPLOY_ADMISSION_POLL_INTERVAL,
  type EnvironmentDeployEventData,
  executeProcessEnvironmentDeployment,
  executeProcessEnvironmentDeploymentOnFailure,
  executeMarkCancelledRowBackedWorkflow,
  PROCESS_ENVIRONMENT_DEPLOYMENT_CONCURRENCY,
  type EnvironmentDeploymentStepTools,
} from "./environment-deployment.inngest";
import {
  PloyzProviderError,
  SdkSurfaceNotShipped,
} from "#/modules/runtime/ployz.server";
import type { DeploymentContext } from "#/modules/deployments/runtime-repository.server";
import * as runtimeCancellation from "#/modules/deployments/runtime-cancellation.repository.server";
import * as runtimeHydration from "#/modules/deployments/runtime-hydration.repository.server";
import * as runtimeLifecycle from "#/modules/deployments/runtime-lifecycle.repository.server";
import * as runtimeActivities from "#/modules/deployments/runtime-activities.server";
import {
  createImageServiceSource,
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import {
  DeploymentRuntimeInvalid,
} from "#/modules/deployments/runtime-activities.server";

const sdkPreview = {
  project_name: "production",
  operations: [
    {
      index: 0,
      machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      operation: {
        type: "run_container",
        machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        spec: {},
        skip_health_monitor: false,
      },
      status: { type: "pending" },
    },
  ],
  warnings: [],
  would_remove: [],
  preserved_volumes: [],
};

const mocks = {
  loadDeploymentContext: vi.fn(),
  persistDeployApplyResult: vi.fn(),
  recordInngestRun: vi.fn(),
  ownsDeploymentRun: vi.fn(),
  markDeploymentFailedIfOwned: vi.fn(),
  markDeploymentStatus: vi.fn(),
  beginEnvironmentDeploymentPlanning: vi.fn(),
  markCancelledByInngestRunId: vi.fn(),
  executeEnvironmentDeployment: vi.fn(),
};

function runtimeFailure(
  operation: "preview" | "confirm",
  cause: unknown,
) {
  if (
    cause instanceof DeploymentRuntimeInvalid ||
    cause instanceof PloyzProviderError ||
    cause instanceof SdkSurfaceNotShipped
  ) {
    return cause;
  }
  return new PloyzProviderError({ operation, cause });
}

vi.spyOn(
  runtimeHydration,
  "loadDeploymentContext",
).mockImplementation((id) =>
  Effect.promise(() => mocks.loadDeploymentContext(id)),
);
vi.spyOn(
  runtimeLifecycle,
  "recordInngestRun",
).mockImplementation((input) =>
  Effect.promise(() => mocks.recordInngestRun(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "beginEnvironmentDeploymentPlanning",
).mockImplementation((input) =>
  Effect.promise(() => mocks.beginEnvironmentDeploymentPlanning(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "markDeploymentStatus",
).mockImplementation((input) =>
  Effect.promise(() => mocks.markDeploymentStatus(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "persistDeployApplyResult",
).mockImplementation((input) =>
  Effect.promise(() => mocks.persistDeployApplyResult(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "ownsDeploymentRun",
).mockImplementation((input) =>
  Effect.promise(() => mocks.ownsDeploymentRun(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "markDeploymentFailedIfOwned",
).mockImplementation((input) =>
  Effect.promise(() => mocks.markDeploymentFailedIfOwned(input)),
);
vi.spyOn(
  runtimeCancellation,
  "markCancelledByInngestRunId",
).mockImplementation((runId, message) =>
  Effect.promise(() => message === undefined ? mocks.markCancelledByInngestRunId(runId) : mocks.markCancelledByInngestRunId(runId, message)),
);
vi.spyOn(
  runtimeActivities,
  "executeLatestEnvironmentDeployment",
).mockImplementation(() =>
  Effect.tryPromise({
    try: () => mocks.executeEnvironmentDeployment(),
    catch: (cause) => runtimeFailure("confirm", cause),
  }),
);

function createStepTools({
  wrapErrors = false,
}: { wrapErrors?: boolean } = {}): EnvironmentDeploymentStepTools {
  const run: EnvironmentDeploymentStepTools["run"] = async (
    id,
    operation,
    ...input
  ) => {
    try {
      const result = await operation(...input);
      return result === undefined ? null : JSON.parse(JSON.stringify(result));
    } catch (error) {
      if (!wrapErrors) throw error;
      throw new StepError(
        String(id),
        serializeError(error),
      );
    }
  };
  return {
    run,
    sleep: vi.fn(async () => undefined),
    sendEvent: vi.fn(async () => ({ ids: [] })),
  };
}

interface InngestTestEvent {
  name?: string;
  data?: EnvironmentDeployEventData;
}

async function runDeploy(payload: {
  event?: InngestTestEvent;
  step?: EnvironmentDeploymentStepTools;
  runId?: string;
}) {
  const event = payload.event;
  const normalizedEvent =
    event && event.name === undefined
      ? { ...event, name: "environment/deploy.requested" }
      : (event ?? { name: "environment/deploy.requested", data: {} });
  return executeProcessEnvironmentDeployment(
    {
      event: {
        name: normalizedEvent.name,
        data: normalizedEvent.data ?? {},
      },
      step: payload.step ?? createStepTools(),
      runId: payload.runId ?? "run-1",
    },
  );
}

function createDeploymentContext(
  status: DeploymentContext["deployment"]["status"] = "queued",
): DeploymentContext {
  return {
    deployment: {
      id: "deployment-1",
      status,
      environmentId: "env-1",
      coreDeployId: null,
      inngestRunId: null,
      deployPreview: sdkPreview,
    },
    environment: {
      id: "env-1",
      namespace: "production",
    },
    project: {
      id: "project-1",
      organizationId: "org-1",
    },
    organization: {
      id: "org-1",
      slug: "nick",
    },
    snapshots: [
      {
        serviceId: "service-1",
        serviceSlug: "api",
        config: projectServiceDeploymentConfig({
          name: "API",
          privateDns: "api",
          source: createImageServiceSource({
            image: "nginx:1.27",
          }),
          preDeployCommand: null,
          startCommand: null,
          healthcheck: createDefaultServiceHealthcheck(),
          restartPolicy: createDefaultServiceRestartPolicy(),
        }),
      },
    ],
    volumes: [],
  };
}

describe("process environment deployment", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.recordInngestRun.mockResolvedValue(true);
    mocks.ownsDeploymentRun.mockResolvedValue(true);
    mocks.markDeploymentFailedIfOwned.mockImplementation(async (input) => {
      const { expectedInngestRunId: _expectedInngestRunId, ...failure } = input;
      await mocks.markDeploymentStatus({ ...failure, status: "failed" });
      return true;
    });
    mocks.persistDeployApplyResult.mockResolvedValue(true);
    mocks.markDeploymentStatus.mockResolvedValue(true);
    mocks.beginEnvironmentDeploymentPlanning.mockResolvedValue({
      state: "started",
    });
    mocks.executeEnvironmentDeployment.mockResolvedValue({ type: "success", completed: 0 });
  });

  it("executes the admitted plan and writes Applied from success", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    const step = createStepTools();
    const result = await runDeploy({
      event: { data: { environmentDeploymentId: "deployment-1" } },
      step,
    });

    expect(mocks.executeEnvironmentDeployment).toHaveBeenCalledTimes(1);
    expect(mocks.loadDeploymentContext).toHaveBeenCalledTimes(1);
    expect(mocks.persistDeployApplyResult).toHaveBeenCalledWith({
      environmentDeploymentId: "deployment-1",
      result: { coreDeployId: null },
    });
    expect(result).toEqual({
      environmentDeploymentId: "deployment-1",
      status: "applied",
    });
  });

  it("fails preview with SdkSurfaceNotShipped without writing Applied", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    mocks.executeEnvironmentDeployment.mockRejectedValue(
      new SdkSurfaceNotShipped({
        surface: "preview",
        ticket: "getployz/ployz2#253",
      }),
    );

    await expect(
      runDeploy({
        event: { data: { environmentDeploymentId: "deployment-1" } },
      }),
    ).rejects.toBeInstanceOf(NonRetriableError);
    expect(mocks.persistDeployApplyResult).not.toHaveBeenCalled();
    expect(mocks.markDeploymentFailedIfOwned).toHaveBeenCalledWith(
      expect.objectContaining({
        failureCode: "sdk_surface_not_shipped",
      }),
    );
  });

  it("does not write Applied from a failed DeployOutcome", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    mocks.executeEnvironmentDeployment.mockResolvedValue({ type: "failed", completed: 1, unexecuted: 2, reason: "machine" });

    await expect(
      runDeploy({
        event: { data: { environmentDeploymentId: "deployment-1" } },
      }),
    ).rejects.toBeInstanceOf(NonRetriableError);
    expect(mocks.persistDeployApplyResult).not.toHaveBeenCalled();
    expect(mocks.markDeploymentFailedIfOwned).toHaveBeenCalledWith(expect.objectContaining({
      failureCode: "sdk_deploy_failed",
      message: "Deployment stopped (machine): 1 operations completed; 2 not attempted. The failed operation may have additional effects.",
    }));
  });

  it("terminalizes a non-retriable typed activity failure", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    mocks.executeEnvironmentDeployment.mockRejectedValue(
      new DeploymentRuntimeInvalid({
        failureCode: "deploy_image_not_pullable",
        message: "Git sources are not pullable.",
      }),
    );

    await expect(
      runDeploy({
        event: { data: { environmentDeploymentId: "deployment-1" } },
      }),
    ).rejects.toBeInstanceOf(NonRetriableError);
    expect(mocks.persistDeployApplyResult).not.toHaveBeenCalled();
    expect(mocks.markDeploymentFailedIfOwned).toHaveBeenCalledWith(
      expect.objectContaining({
        failureCode: "deploy_image_not_pullable",
      }),
    );
  });

  it("records a cancelled runtime outcome with its partial counts", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    mocks.executeEnvironmentDeployment.mockResolvedValue({
      type: "failed", completed: 1, unexecuted: 2, reason: "cancelled",
    });
    mocks.markCancelledByInngestRunId.mockResolvedValue(true);
    const result = await runDeploy({ event: { data: { environmentDeploymentId: "deployment-1" } } });
    expect(result).toEqual({ environmentDeploymentId: "deployment-1", status: "cancelled" });
    expect(mocks.markCancelledByInngestRunId).toHaveBeenCalledWith("run-1", expect.stringContaining("1 operations completed; 2 not attempted"));
    expect(mocks.persistDeployApplyResult).not.toHaveBeenCalled();
  });

  it("skips a terminal deployment", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(
      createDeploymentContext("applied"),
    );
    const result = await runDeploy({
      event: { data: { environmentDeploymentId: "deployment-1" } },
    });
    expect(result).toEqual({
      environmentDeploymentId: "deployment-1",
      status: "applied",
      skipped: true,
    });
    expect(mocks.executeEnvironmentDeployment).not.toHaveBeenCalled();
  });

  it("skips a run owned elsewhere", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    mocks.recordInngestRun.mockResolvedValue(false);
    const result = await runDeploy({
      event: { data: { environmentDeploymentId: "deployment-1" } },
    });
    expect(result).toEqual({
      environmentDeploymentId: "deployment-1",
      status: "owned_elsewhere",
      skipped: true,
    });
  });

  it("waits for an active deployment before planning", async () => {
    mocks.loadDeploymentContext.mockResolvedValue(createDeploymentContext());
    mocks.beginEnvironmentDeploymentPlanning
      .mockResolvedValueOnce({ state: "blocked" })
      .mockResolvedValue({ state: "started" });
    const step = createStepTools();
    await runDeploy({
      event: { data: { environmentDeploymentId: "deployment-1" } },
      step,
    });
    expect(step.sleep).toHaveBeenCalledWith(
      "wait-for-active-deployment",
      DEPLOY_ADMISSION_POLL_INTERVAL,
    );
  });

  it.each([
    { status: "planning" as const, message: "retry budget exhausted", failureCode: undefined },
    { status: "deploying" as const, message: "Runtime execution ended without a complete outcome; effects are unknown.", failureCode: "sdk_deploy_outcome_unknown" },
  ])("onFailure reports $status without writing Applied", async ({ status, message, failureCode }) => {
    const context = createDeploymentContext(status);
    context.deployment.inngestRunId = "run-1";
    mocks.loadDeploymentContext.mockResolvedValue(context);
    await executeProcessEnvironmentDeploymentOnFailure(
      {
        event: {
          name: "inngest/function.failed",
          data: {
            function_id: "process-environment-deployment",
            run_id: "run-1",
            error: {
              name: "Error",
              message: "retry budget exhausted",
            },
            event: {
              name: "environment/deploy.requested",
              data: {
                environmentDeploymentId: "deployment-1",
                environmentId: "environment-1",
              },
            },
          },
        },
        error: new Error("retry budget exhausted"),
      },
    );

    expect(mocks.persistDeployApplyResult).not.toHaveBeenCalled();
    expect(mocks.markDeploymentFailedIfOwned).toHaveBeenCalledWith({
      environmentDeploymentId: "deployment-1",
      expectedInngestRunId: "run-1",
      message,
      failureCode,
    });
  });

  it("keeps one in-flight deploy per environment", () => {
    expect(PROCESS_ENVIRONMENT_DEPLOYMENT_CONCURRENCY).toEqual([
      { key: "event.data.environmentId", limit: 1 },
    ]);
  });

  it("marks the durable row cancelled when Inngest cancels the run", async () => {
    mocks.markCancelledByInngestRunId.mockResolvedValue(true);
    const step = createStepTools();
    const result = await executeMarkCancelledRowBackedWorkflow(
      {
        event: {
          data: {
            function_id: "process-environment-deployment",
            run_id: "run-1",
          },
        },
        step,
      },
      (effect) =>
        Effect.runPromise(effect.pipe(
          Effect.provide(AppConfig.layer),
          // SAFETY: The deployment cancellation branch never performs GitHub persistence.
          Effect.provideService(Database, undefined as never),
          // SAFETY: The deployment cancellation branch never sends Inngest events.
          Effect.provideService(InngestClient, undefined as never),
          // SAFETY: The deployment cancellation branch never calls GitHub.
          Effect.provideService(GithubApi, undefined as never),
        )),
    );

    expect(mocks.markCancelledByInngestRunId).toHaveBeenCalledWith("run-1");
    expect(result).toEqual({
      functionId: "process-environment-deployment",
      runId: "run-1",
      marked: true,
    });
  }, 10_000);
});
