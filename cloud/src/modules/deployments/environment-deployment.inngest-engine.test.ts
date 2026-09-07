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
import {
  PloyzProviderError,
  SdkSurfaceNotShipped,
} from "#/modules/runtime/ployz.server";
import { createProcessEnvironmentDeployment } from "./environment-deployment.inngest";

const activity = {
  claim: vi.fn(),
  load: vi.fn(),
  planning: vi.fn(),
  preview: vi.fn(),
  apply: vi.fn(),
  confirm: vi.fn(),
  persistSuccess: vi.fn(),
  authorizeFailure: vi.fn(),
  terminalizeFailure: vi.fn(),
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
vi.spyOn(runtimeActivities, "previewEnvironmentDeployment").mockImplementation(
  (context) =>
    Effect.tryPromise({
      try: () => activity.preview(context),
      catch: (cause) => runtimeFailure("preview", cause),
    }),
);
vi.spyOn(
  runtimeActivities,
  "confirmLatestEnvironmentDeployment",
).mockImplementation(() =>
  Effect.tryPromise({
    try: () => activity.confirm(),
    catch: (cause) => runtimeFailure("confirm", cause),
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
    activity.preview.mockResolvedValue(undefined);
    activity.apply.mockResolvedValue(true);
    activity.confirm.mockResolvedValue({
      type: "success",
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
        retries: 3,
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

  it("registers the exact confirmation wait before runtime application", async () => {
    const output = await makeEngine().executeStep("wait-for-deploy-confirm");

    expect(output.step).toEqual(
      expect.objectContaining({
        displayName: "wait-for-deploy-confirm",
        name: "environment/deploy.confirmed",
        opts: expect.objectContaining({
          timeout: "1h",
          if: "event.data.environmentDeploymentId == async.data.environmentDeploymentId",
        }),
        userland: { id: "wait-for-deploy-confirm" },
      }),
    );
    expect(activity.preview).toHaveBeenCalledTimes(1);
    expect(activity.confirm).not.toHaveBeenCalled();
  });

  it("marks typed deterministic activity failures as non-retriable", async () => {
    activity.preview.mockRejectedValue(
      new DeploymentRuntimeInvalid({
        failureCode: "deploy_image_not_pullable",
        message: "Git sources are not pullable.",
      }),
    );

    const output = await makeEngine().executeStep("preview-sdk-deploy");

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Git sources are not pullable.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
  });

  it("leaves provider failures retriable at the step boundary", async () => {
    activity.preview.mockRejectedValue(
      new PloyzProviderError({ operation: "connect", cause: "offline" }),
    );

    const output = await makeEngine().executeStep("preview-sdk-deploy");

    expect(output.error).toEqual(
      expect.objectContaining({ name: "PloyzProviderError" }),
    );
  });
});
