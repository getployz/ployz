import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { environmentDeployRequestedEventType } from "#/modules/inngest/events";
import type { DeploymentContext } from "#/modules/deployments/runtime-repository.server";
import * as runtimeHydration from "#/modules/deployments/runtime-hydration.repository.server";
import * as runtimeLifecycle from "#/modules/deployments/runtime-lifecycle.repository.server";
import * as runtimeActivities from "#/modules/deployments/runtime-activities.server";
import {
  DeploymentRuntimeInvalid,
  type DeploymentRuntimeOutcome,
} from "#/modules/deployments/runtime-activities.server";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { createProcessEnvironmentDeployment } from "./environment-deployment.inngest";

const activity = {
  claim: vi.fn(),
  load: vi.fn(),
  planning: vi.fn(),
  apply: vi.fn(),
  execute: vi.fn(),
  persistSuccess: vi.fn(),
  authorizeFailure: vi.fn(),
  terminalizeFailure: vi.fn(),
};

function runtimeFailure(
  operation: "execute",
  cause: unknown,
) {
  if (
    cause instanceof DeploymentRuntimeInvalid ||
    cause instanceof PloyzProviderError
  ) {
    return cause;
  }
  return new PloyzProviderError({ operation, cause });
}

vi.spyOn(
  runtimeLifecycle,
  "recordInngestRun",
).mockImplementation((input) => Effect.promise(() => activity.claim(input)));
vi.spyOn(
  runtimeHydration,
  "loadDeploymentContext",
).mockImplementation((environmentDeploymentId) =>
  Effect.promise(() => activity.load(environmentDeploymentId)),
);
vi.spyOn(
  runtimeLifecycle,
  "beginEnvironmentDeploymentPlanning",
).mockImplementation((input) =>
  Effect.promise(() => activity.planning(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "markDeploymentStatus",
).mockImplementation((input) => Effect.promise(() => activity.apply(input)));
vi.spyOn(
  runtimeLifecycle,
  "persistDeployApplyResult",
).mockImplementation((input) =>
  Effect.promise(() => activity.persistSuccess(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "ownsDeploymentRun",
).mockImplementation((input) =>
  Effect.promise(() => activity.authorizeFailure(input)),
);
vi.spyOn(
  runtimeLifecycle,
  "markDeploymentFailedIfOwned",
).mockImplementation((input) =>
  Effect.promise(() => activity.terminalizeFailure(input)),
);
vi.spyOn(
  runtimeActivities,
  "executeLatestEnvironmentDeployment",
).mockImplementation(() =>
  Effect.tryPromise({
    try: () => activity.execute(),
    catch: (cause) => runtimeFailure("execute", cause),
  }),
);

const deploymentContext = {
  deployment: {
    id: "deployment-1",
    environmentId: "environment-1",
    status: "queued",
    inngestRunId: null,
  },
  environment: { id: "environment-1", namespace: "production" },
  project: { id: "project-1", organizationId: "organization-1" },
  organization: {
    id: "organization-1",
    slug: "organization",
  },
  snapshots: [],
  appliedServiceIds: [],
  volumes: [],
} satisfies DeploymentContext;

function makeEngine() {
  return new InngestTestEngine({
    function: createProcessEnvironmentDeployment(new Inngest({ id: "test" })),
    events: [
      {
        name: "environment/deploy.requested",
        data: {
          environmentDeploymentId: "deployment-1",
          environmentId: "environment-1",
        },
      },
    ],
  });
}

describe("process-environment-deployment Inngest adapter", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    activity.claim.mockResolvedValue(true);
    activity.load.mockResolvedValue(deploymentContext);
    activity.planning.mockResolvedValue({ state: "started" });
    activity.apply.mockResolvedValue(true);
    activity.execute.mockResolvedValue({
      type: "success", completed: 0,
    } satisfies DeploymentRuntimeOutcome);
    activity.persistSuccess.mockResolvedValue(true);
    activity.authorizeFailure.mockResolvedValue(true);
    activity.terminalizeFailure.mockResolvedValue(true);
  });

  it("preserves the durable trigger, retries, and keyed concurrency", () => {
    const processEnvironmentDeployment = createProcessEnvironmentDeployment(
      new Inngest({ id: "test" }),
    );
    expect(processEnvironmentDeployment.opts).toEqual(
      expect.objectContaining({
        id: "process-environment-deployment",
        retries: 0,
        triggers: [{ event: environmentDeployRequestedEventType }],
        concurrency: [{ key: "event.data.environmentId", limit: 1 }],
      }),
    );
  });

  it("rejects an incomplete event envelope inside the decode step", async () => {
    const output = await new InngestTestEngine({
      function: createProcessEnvironmentDeployment(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "environment/deploy.requested",
          data: { environmentDeploymentId: "deployment-1" },
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
    expect(activity.claim).not.toHaveBeenCalled();
  });

  it("executes the admitted target without another confirmation wait", async () => {
    const output = await makeEngine().execute();
    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ environmentDeploymentId: "deployment-1", status: "applied" });
    expect(output.ctx.step.waitForEvent).not.toHaveBeenCalled();
    expect(activity.execute).toHaveBeenCalledTimes(1);
    expect(activity.persistSuccess).toHaveBeenCalledTimes(1);
  });

  it("marks typed deterministic activity failures as non-retriable", async () => {
    activity.execute.mockRejectedValue(
      new DeploymentRuntimeInvalid({
        failureCode: "deploy_image_not_pullable",
        message: "Git sources are not pullable.",
      }),
    );

    const output = await makeEngine().executeStep("execute-sdk-deploy");

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Git sources are not pullable.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
  });

  it("preserves typed provider failures with automatic workflow retries disabled", async () => {
    activity.execute.mockRejectedValue(
      new PloyzProviderError({ operation: "connect", cause: "offline" }),
    );

    const output = await makeEngine().executeStep("execute-sdk-deploy");

    expect(output.error).toEqual(
      expect.objectContaining({ name: "PloyzProviderError" }),
    );
  });
});
