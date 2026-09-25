import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { environmentDeployRequestedEventType } from "#/modules/inngest/events";
import type { DeploymentContext } from "#/modules/deployments/runtime-repository.server";
import * as runtimeHydration from "#/modules/deployments/runtime-hydration.repository.server";
import * as runtimeLifecycle from "#/modules/deployments/runtime-lifecycle.repository.server";
import * as runtimeActivities from "#/modules/deployments/runtime-activities.server";
import type { DeploymentRuntimeOutcome } from "#/modules/deployments/runtime-activities.server";
import { DeploymentRuntimeInvalid } from "#/modules/deployments/runtime-session.server";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import * as imageBuilds from "#/modules/deployments/image-builds.server";
import * as buildOrder from "#/modules/deployments/build-order.server";
import { imageBuildWalk } from "#/modules/deployments/build-order";
import * as serverImageBuilds from "#/modules/deployments/server-image-builds.server";
import type { ImageBuildTarget } from "#/modules/deployments/image-builds.server";
import { createProcessEnvironmentDeployment, IMAGE_BUILDS_AT_ONCE } from "./environment-deployment.inngest";

const activity = {
  claim: vi.fn(),
  load: vi.fn(),
  planning: vi.fn(),
  execute: vi.fn(),
  authorizeFailure: vi.fn(),
  terminalizeFailure: vi.fn(),
  startBuilds: vi.fn(),
  build: vi.fn(),
};

const build = (image: string, buildIndex = 0): ImageBuildTarget => ({ id: `build-${image}`, deploymentId: "deployment-1", serviceId: `service-${image}`, image, buildIndex });

vi.spyOn(imageBuilds, "startImageBuilds").mockImplementation(() => Effect.promise(() => activity.startBuilds()));
vi.spyOn(serverImageBuilds, "buildOnServers").mockImplementation((target) =>
  Effect.promise(async () => ({ kind: "settled" as const, result: await activity.build(target) })));
vi.spyOn(buildOrder, "planImageBuildWalk").mockImplementation(() => Effect.succeed(imageBuildWalk("servers-only", undefined)));

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

useServiceFreeEffectRunner();

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

it("matches cancellation to the exact deployment", () => {
  const fn = createProcessEnvironmentDeployment(new Inngest({ id: "cancel-test" }));
  expect(fn.opts.cancelOn).toEqual([{
    event: "environment/deploy.cancel.requested", match: "data.environmentDeploymentId",
  }]);
});

describe("process-environment-deployment Inngest adapter", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    activity.claim.mockResolvedValue(true);
    activity.startBuilds.mockResolvedValue([]);
    activity.load.mockResolvedValue(deploymentContext);
    activity.planning.mockResolvedValue({ state: "started" });
    activity.execute.mockImplementation(async () => {
      activity.load.mockResolvedValue({ ...deploymentContext, deployment: { ...deploymentContext.deployment, status: "applied" } });
      return { type: "success", completed: 0 } satisfies DeploymentRuntimeOutcome;
    });
    activity.authorizeFailure.mockResolvedValue(true);
    activity.terminalizeFailure.mockResolvedValue(true);
  });

  it("preserves the durable trigger and retries, limiting concurrency per attempt so Image Builds run side by side", () => {
    const processEnvironmentDeployment = createProcessEnvironmentDeployment(
      new Inngest({ id: "test" }),
    );
    expect(processEnvironmentDeployment.opts).toEqual(
      expect.objectContaining({
        id: "process-environment-deployment",
        retries: 0,
        triggers: [{ event: environmentDeployRequestedEventType }],
      }),
    );
    expect(processEnvironmentDeployment.opts.concurrency).toEqual([{ key: "event.data.environmentDeploymentId", limit: IMAGE_BUILDS_AT_ONCE }]);
  });

  it("starts every Image Build before taking the Environment slot, then deploys", async () => {
    const order: string[] = [];
    activity.startBuilds.mockResolvedValue([build("api"), build("web")]);
    activity.build.mockImplementation(async ({ image }: ImageBuildTarget) => {
      order.push(`build ${image}`);
      return { imageBuildId: image, image, status: "built" };
    });
    activity.planning.mockImplementation(async () => {
      order.push("planning");
      return { state: "started" };
    });
    const output = await makeEngine().execute();
    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ environmentDeploymentId: "deployment-1", status: "applied" });
    // The test engine resumes once per parallel branch, so it may replay steps after the fan-out.
    expect(new Set(order.slice(0, 2))).toEqual(new Set(["build api", "build web"]));
    expect(order).toContain("planning");
    expect(activity.execute).toHaveBeenCalled();
  });

  it("lets every Image Build settle, then fails the attempt without taking the slot", async () => {
    activity.startBuilds.mockResolvedValue([build("api"), build("web")]);
    activity.build.mockImplementation(async ({ image }: ImageBuildTarget) =>
      ({ imageBuildId: image, image, status: image === "api" ? "failed" : "built" }));
    const output = await makeEngine().execute();
    expect(output.error).toEqual(expect.objectContaining({ message: "Image Build failed: api." }));
    // SAFETY: buildOnServers' spy passes each build its ImageBuildTarget.
    const images = activity.build.mock.calls.map(([target]) => (target as ImageBuildTarget).image);
    // The test engine may replay a build step; each image still built.
    expect(new Set(images)).toEqual(new Set(["api", "web"]));
    expect(activity.planning).not.toHaveBeenCalled();
    expect(activity.execute).not.toHaveBeenCalled();
    expect(activity.terminalizeFailure).toHaveBeenCalledWith(expect.objectContaining({
      environmentDeploymentId: "deployment-1", failureCode: "image_build_failed",
    }));
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
  });

  it("marks typed deterministic activity failures as non-retriable", async () => {
    activity.execute.mockRejectedValue(
      new DeploymentRuntimeInvalid({
        failureCode: "sdk_preview_invalid",
        message: "Runtime preview is invalid.",
      }),
    );

    const output = await makeEngine().executeStep("execute-sdk-deploy");

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Runtime preview is invalid.",
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
