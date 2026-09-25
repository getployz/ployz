import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { MachineId } from "@ployz/sdk";
import { imageBuildWalk, type BuildCandidate } from "#/modules/deployments/build-order";
import type { SkipReason } from "#/modules/deployments/image-build";
import * as buildOrder from "#/modules/deployments/build-order.server";
import * as githubImageBuilds from "#/modules/deployments/github-image-builds.server";
import * as imageBuilds from "#/modules/deployments/image-builds.server";
import type { ImageBuildAttempt, ImageBuildOutcome, ImageBuildTarget } from "#/modules/deployments/image-builds.server";
import * as runtimeActivities from "#/modules/deployments/runtime-activities.server";
import * as serverImageBuilds from "#/modules/deployments/server-image-builds.server";
import * as runtimeHydration from "#/modules/deployments/runtime-hydration.repository.server";
import * as runtimeLifecycle from "#/modules/deployments/runtime-lifecycle.repository.server";
import type { DeploymentContext } from "#/modules/deployments/runtime-repository.server";
import { createProcessEnvironmentDeployment } from "./environment-deployment.inngest";

/**
 * Walking the Build Order, with each Builder faked at its activity: which Builder ends up with the
 * build, what each is allowed to wait, and where the walk stops.
 */

const target: ImageBuildTarget = { id: "build-api", deploymentId: "deployment-1", serviceId: "service-api", image: "api", buildIndex: 0 };
const settled = (status: "built" | "failed"): ImageBuildAttempt => ({ kind: "settled", result: { imageBuildId: target.id, image: "api", status } });
const skipped = (reason: SkipReason): ImageBuildAttempt => ({ kind: "skipped", reason });
const serversNotStarted: SkipReason = { builder: "servers", kind: "not_started", minutes: 3 };
const START_WITHIN_MS = imageBuilds.START_WITHIN_MINUTES * 60_000;
const RUN_ID = 77;

const fake = {
  candidates: [] as BuildCandidate[],
  /** Each server go: what it returns, and the start limit it was given. */
  servers: [] as ImageBuildAttempt[],
  serverLimits: [] as (number | undefined)[],
  serverPreferred: [] as (MachineId | undefined)[],
  githubReasons: [] as BuildCandidate["reason"][],
  githubStart: null as githubImageBuilds.GithubBuildStart | null,
  /** The Workflow run webhook: the run's completion, or nothing before the wait's timeout. */
  runCompletes: [] as boolean[],
  waits: [] as (string | number | undefined)[],
  answered: new Map<string, boolean>(),
  /** Each look at the dispatched run: what it finds, and what the walk told it. */
  checks: [] as githubImageBuilds.GithubBuildCheck[],
  seen: [] as { ended: boolean; startLimit: boolean }[],
  failed: [] as string[],
  deployed: 0,
};

useServiceFreeEffectRunner();
vi.spyOn(runtimeLifecycle, "recordInngestRun").mockImplementation(() => Effect.succeed(true));
vi.spyOn(runtimeLifecycle, "beginEnvironmentDeploymentPlanning").mockImplementation(() => Effect.succeed({ state: "started" }));
vi.spyOn(runtimeLifecycle, "ownsDeploymentRun").mockImplementation(() => Effect.succeed(true));
vi.spyOn(runtimeLifecycle, "markDeploymentFailedIfOwned").mockImplementation(() => Effect.succeed(true));
const context = {
  deployment: { id: "deployment-1", environmentId: "environment-1", status: "queued", inngestRunId: null },
  environment: { id: "environment-1", namespace: "production" },
  project: { id: "project-1", organizationId: "organization-1" },
  organization: { id: "organization-1", slug: "organization" },
  snapshots: [], appliedServiceIds: [], volumes: [],
} satisfies DeploymentContext;
const loadContext = vi.fn();
loadContext.mockImplementation(async () => fake.deployed ? { ...context, deployment: { ...context.deployment, status: "applied" } } : context);
vi.spyOn(runtimeHydration, "loadDeploymentContext").mockImplementation(() => Effect.promise(() => loadContext()));
vi.spyOn(runtimeActivities, "executeLatestEnvironmentDeployment").mockImplementation(() => Effect.sync(() => {
  fake.deployed += 1;
  return { type: "success", completed: 0 } as const;
}));
vi.spyOn(imageBuilds, "startImageBuilds").mockImplementation(() => Effect.succeed([target]));
vi.spyOn(buildOrder, "planImageBuildWalk").mockImplementation(() => Effect.sync(() => fake.candidates));
vi.spyOn(serverImageBuilds, "buildOnServers").mockImplementation((_build, candidate, startWithinMs) => Effect.sync(() => {
  fake.serverLimits.push(startWithinMs);
  fake.serverPreferred.push(candidate.machineId);
  return fake.servers.shift() ?? settled("built");
}));
vi.spyOn(githubImageBuilds, "startGithubImageBuild").mockImplementation((_build, candidate) => Effect.sync(() => {
  fake.githubReasons.push(candidate.reason);
  return fake.githubStart ?? { kind: "dispatched", runId: RUN_ID };
}));
// Nothing settles a build between two waits here; the Postgres tests cover a report that did.
vi.spyOn(githubImageBuilds, "settledGithubImageBuild").mockImplementation(() => Effect.succeed(null));
vi.spyOn(githubImageBuilds, "checkGithubImageBuild").mockImplementation((_build, seen) => Effect.sync(() => {
  fake.seen.push(seen);
  return fake.checks.shift() ?? (seen.ended ? settled("built") : { kind: "waiting" });
}));
vi.spyOn(imageBuilds, "settleImageBuild").mockImplementation((build, outcome: ImageBuildOutcome) => Effect.sync(() => {
  if (outcome.status === "failed") fake.failed.push(outcome.message);
  return imageBuilds.settled(build, outcome.status);
}));

function walk() {
  return new InngestTestEngine({
    function: createProcessEnvironmentDeployment(new Inngest({ id: "build-order" })),
    events: [{ name: "environment/deploy.requested", data: { environmentDeploymentId: "deployment-1", environmentId: "environment-1" } }],
    transformCtx: (ctx) => {
      const mocked = mockCtx(ctx);
      // @inngest/test can't mock waitForEvent (inngest 4 validates its lazy promise as an event).
      const waitForEvent = async (id: string, options: { timeout?: string | number }) => {
        // The replaced tool isn't memoized, and the engine replays: answer each wait once.
        if (!fake.answered.has(id)) {
          fake.waits.push(options.timeout);
          fake.answered.set(id, fake.runCompletes.shift() ?? false);
        }
        return fake.answered.get(id) ? { name: "github/build-run.completed", data: { runId: RUN_ID } } : null;
      };
      return { ...mocked, step: { ...mocked.step, waitForEvent: asTestDouble<typeof mocked.step.waitForEvent>()(waitForEvent) } };
    },
  });
}

/** Runs the attempt: whether it failed, and whether it went on to deploy. */
const outcome = async () => {
  const output = await walk().execute();
  return { error: output.error, deployed: fake.deployed > 0 };
};

describe("walking the Build Order", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.assign(fake, {
      candidates: [], servers: [], serverLimits: [], serverPreferred: [], githubReasons: [], githubStart: null, runCompletes: [], waits: [], answered: new Map(),
      checks: [], seen: [], failed: [], deployed: 0,
    });
  });

  it("gives a lone Builder no start limit: your servers only waits in the queue", async () => {
    fake.candidates = imageBuildWalk("servers-only", undefined);
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(new Set(fake.serverLimits)).toEqual(new Set([undefined]));
  });

  it("tells GitHub why it is in the walk, so it records the reason when it takes the build", async () => {
    fake.candidates = imageBuildWalk("servers-then-github", "github");
    expect(fake.candidates).toEqual([{ builder: "github", reason: "preferred" }, { builder: "servers", reason: "first_in_build_order" }]);
    fake.runCompletes = [true];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.githubReasons).toEqual(["preferred"]);
  });

  it("hands a build the servers didn't start in time to GitHub, whose last place waits for the run", async () => {
    fake.candidates = imageBuildWalk("servers-then-github", undefined);
    fake.servers = [skipped(serversNotStarted)];
    fake.runCompletes = [true];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.serverLimits[0]).toBe(START_WITHIN_MS);
    expect(fake.waits).toEqual(["10m"]);
  });

  it("lets GitHub in last place wait for its run to start without a limit", async () => {
    fake.candidates = imageBuildWalk("github-only", undefined);
    fake.runCompletes = [false, false, false, true];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.waits).toEqual(["10m", "10m", "10m", "10m"]);
    expect(fake.seen.some(({ startLimit }) => startLimit)).toBe(false);
  });

  it("settles a run whose completion no webhook delivered once a check finds it on GitHub", async () => {
    fake.candidates = imageBuildWalk("github-only", undefined);
    fake.runCompletes = [false];
    fake.checks = [settled("built")];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.seen).toEqual([{ ended: false, startLimit: false }]);
  });

  it("falls to the servers, which then wait, when no GitHub runner checks in within the limit", async () => {
    fake.candidates = imageBuildWalk("github-then-servers", undefined);
    fake.runCompletes = [false];
    fake.checks = [skipped({ builder: "github", kind: "not_started", minutes: 3 })];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.waits[0]).toBe("3m");
    expect(fake.seen[0]).toEqual({ ended: false, startLimit: true });
    expect(new Set(fake.serverLimits)).toEqual(new Set([undefined]));
  });

  it("skips GitHub at once when it can't take the build", async () => {
    fake.candidates = imageBuildWalk("github-then-servers", undefined);
    fake.githubStart = skipped({ builder: "github", kind: "multi_platform", platforms: ["amd64", "arm64"] });
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.waits).toEqual([]);
    expect(fake.serverLimits.length).toBeGreaterThan(0);
  });

  it("moves on when the Workflow run webhook reports the run ended before it checked in", async () => {
    fake.candidates = imageBuildWalk("github-then-servers", undefined);
    fake.runCompletes = [true];
    fake.checks = [skipped({ builder: "github", kind: "ended_before_start" })];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.seen[0]).toEqual({ ended: true, startLimit: true });
    expect(fake.serverLimits.length).toBeGreaterThan(0);
  });

  it("keeps a GitHub run that checked in before the limit, and never moves it when it fails", async () => {
    fake.candidates = imageBuildWalk("github-then-servers", undefined);
    fake.runCompletes = [false, true];
    fake.checks = [{ kind: "waiting" }, settled("failed")];
    const result = await outcome();
    expect(result).toMatchObject({ deployed: false, error: expect.objectContaining({ message: "Image Build failed: api." }) });
    expect(fake.waits).toEqual(["3m", "10m"]);
    expect(fake.serverLimits).toEqual([]);
  });

  it("never moves a server build that started and failed", async () => {
    fake.candidates = imageBuildWalk("servers-then-github", undefined);
    fake.servers = [settled("failed")];
    expect(await outcome()).toMatchObject({ deployed: false });
    expect(githubImageBuilds.startGithubImageBuild).not.toHaveBeenCalled();
  });

  it("asks the Cluster for a Preferred Server first, then walks the Build Order without your servers", async () => {
    const preferred = "a".repeat(32) as MachineId;
    fake.candidates = imageBuildWalk("github-only", preferred);
    fake.servers = [skipped(serversNotStarted)];
    fake.runCompletes = [true];
    expect(await outcome()).toMatchObject({ deployed: true });
    expect(fake.serverLimits[0]).toBe(START_WITHIN_MS);
    expect(new Set(fake.serverPreferred)).toEqual(new Set([preferred]));
    expect(fake.waits).toEqual(["10m"]);
    expect(fake.githubReasons).toEqual(["first_in_build_order"]);
  });

  it("fails the build with the last skip's reason when every Builder was skipped", async () => {
    fake.candidates = imageBuildWalk("github-only", undefined);
    fake.githubStart = skipped({ builder: "github", kind: "no_workflow", repository: "acme/api" });
    expect(await outcome()).toMatchObject({ deployed: false, error: expect.objectContaining({ message: "Image Build failed: api." }) });
    expect(fake.failed).toContain("GitHub: no workflow in acme/api");
  });
});
