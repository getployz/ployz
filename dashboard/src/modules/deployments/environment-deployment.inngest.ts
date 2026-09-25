import { environmentDeployCancelRequestedEvent, githubBuildRunCompletedEvent } from "#/modules/inngest/events";
import { NonRetriableError } from "inngest";
import { Effect, Option, Schema } from "effect";
import {
  environmentDeployRequestedEvent,
  environmentDeployRequestedEventType,
  inngestEventEnvelopeFields,
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  inngestFunctionFailedEnvelopeSchema,
  type EnvironmentDeployRequestedEventData,
  type InngestFunctionCancelledEventData,
} from "#/modules/inngest/events";
import type {
  PloyzInngest,
  PloyzStepTools,
} from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { PROCESS_ENVIRONMENT_DEPLOYMENT_FUNCTION_ID } from "#/modules/inngest/row-backed-workflow-ids";
import { executeCancelGithubRowBackedWorkflow } from "#/modules/inngest/cancellations";
import type { GithubIngestionEffectRunner } from "#/modules/github/inngest-ingestion/process";
import { parseErrorEvidence } from "#/lib/error-evidence";
import { TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/runtime-contract";
import { DeploymentExecutionError } from "#/modules/deployments/execution-error";
import {
  cleanUpDeploymentImages,
  executeLatestEnvironmentDeployment,
} from "#/modules/deployments/runtime-activities.server";
import {
  settleImageBuild,
  START_WITHIN_MINUTES,
  startImageBuilds,
  type ImageBuildAttempt,
  type ImageBuildTarget,
} from "#/modules/deployments/image-builds.server";
import type { BuildCandidate } from "#/modules/deployments/build-order";
import { planImageBuildWalk } from "#/modules/deployments/build-order.server";
import { skipReasonText, type SkipReason } from "#/modules/deployments/image-build";
import { buildOnServers } from "#/modules/deployments/server-image-builds.server";
import {
  cancelGithubImageBuilds,
  checkGithubImageBuild,
  GITHUB_CHECK_INTERVAL,
  settledGithubImageBuild,
  startGithubImageBuild,
} from "#/modules/deployments/github-image-builds.server";
import { markCancelledByInngestRunId } from "#/modules/deployments/runtime-cancellation.repository.server";
import { loadDeploymentContext } from "#/modules/deployments/runtime-hydration.repository.server";
import {
  beginEnvironmentDeploymentPlanning,
  markDeploymentFailedIfOwned,
  ownsDeploymentRun,
  recordInngestRun,
} from "#/modules/deployments/runtime-lifecycle.repository.server";
import type { DeploymentContext } from "#/modules/deployments/runtime-repository.server";
import { runInngestEffect } from "#/server/run.server";

type DeploymentInngestEffectRunner = typeof runInngestEffect;

const NonEmptyString = Schema.Trim.check(Schema.isMinLength(1));
const EnvironmentDeployRequestedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(environmentDeployRequestedEvent),
  data: Schema.Struct({
    environmentDeploymentId: NonEmptyString,
    environmentId: NonEmptyString,
  }),
});
const EnvironmentDeployFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  EnvironmentDeployRequestedEnvelope,
);

type UntrustedInngestEnvelope = {
  readonly name?: unknown;
  readonly data?: unknown;
};

function decodeEnvironmentDeployFailureEnvelope(
  input: UntrustedInngestEnvelope,
) {
  const decoded = Schema.decodeUnknownOption(EnvironmentDeployFailureEnvelope)(
    input,
  );
  return Option.isSome(decoded) ? decoded.value : null;
}

export const DEPLOY_ADMISSION_POLL_INTERVAL = "15s";
/** How many of one attempt's Image Builds run at once; Services beyond that wait their turn. */
export const IMAGE_BUILDS_AT_ONCE = 32;

function deploymentContext<T>(value: T): DeploymentContext | null {
  // SAFETY: Inngest Jsonify-wraps step.run results; loaders return DeploymentContext | null.
  return value as DeploymentContext | null;
}

function isTerminalEnvironmentDeployment(context: DeploymentContext) {
  return TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES.has(context.deployment.status);
}

function deployFailureEvidence(cause: unknown) {
  return parseErrorEvidence(cause instanceof Error ? cause : null);
}

function isDeterministicDeployFailure(cause: unknown) {
  return (
    cause instanceof NonRetriableError ||
    // A NonRetriableError thrown inside `step.run` reaches this catch as an
    // Inngest `StepError`, which keeps only the serialized `name`.
    (cause instanceof Error && cause.name === "NonRetriableError") ||
    cause instanceof DeploymentExecutionError ||
    deployFailureEvidence(cause).failureCode !== undefined
  );
}

function asNonRetriableDeployFailure(cause: unknown) {
  if (cause instanceof NonRetriableError) return cause;
  const evidence = deployFailureEvidence(cause);
  return new NonRetriableError(
    evidence.message ?? "Environment deploy failed.",
    { cause },
  );
}

function terminalizeDeploymentFailure(
  input: {
    readonly environmentDeploymentId: string;
    readonly expectedInngestRunId: string;
    readonly error: unknown;
  },
  runEffect: DeploymentInngestEffectRunner,
) {
  const evidence = deployFailureEvidence(input.error);
  return runEffect(
    markDeploymentFailedIfOwned({
      environmentDeploymentId: input.environmentDeploymentId,
      expectedInngestRunId: input.expectedInngestRunId,
      message: evidence.message ?? "Environment deploy failed.",
      failureCode: evidence.failureCode,
    }),
  );
}

export type EnvironmentDeploymentStepTools = Pick<
  PloyzStepTools,
  "run" | "sleep" | "sendEvent" | "waitForEvent"
>;

/** Where one Builder's go at an Image Build sits in its walk. */
type Walk = { key: string; last: boolean; step: EnvironmentDeploymentStepTools; runEffect: DeploymentInngestEffectRunner };
type Builder = (build: ImageBuildTarget, candidate: BuildCandidate, walk: Walk) => Promise<ImageBuildAttempt>;

/** Your servers: one Cluster build, which is withdrawn unstarted at "start within" unless it is last. */
const walkServers: Builder = (build, candidate, { key, last, step, runEffect }) =>
  step.run(`build-image-${key}`, () => runEffect(buildOnServers(build, candidate, last ? undefined : START_WITHIN_MINUTES * 60_000)));

/**
 * GitHub: dispatch, then wait for the run while the runner checks in and pushes. The runner's final
 * report settles the build and ends a wait at once, as the Workflow run webhook does. Each wait first
 * reads whether the build settled, so a report that landed before the wait began is not missed. Each
 * timeout checks the run on GitHub too, which catches a completion that landed between two waits.
 * Not last: the first check is the "start within" limit, and a run that hasn't checked in by then
 * is withdrawn. Last: it waits for the run to start without a limit.
 */
const walkGithub: Builder = async (build, candidate, { key, last, step, runEffect }) => {
  const started = await step.run(`start-github-build-${key}`, () => runEffect(startGithubImageBuild(build, candidate)));
  if (started.kind !== "dispatched") return started;
  const run = { event: githubBuildRunCompletedEvent, if: `async.data.runId == ${started.runId}` };
  for (let check = 0; ; check += 1) {
    const before = await step.run(`settled-github-build-${key}-${check}`, () => runEffect(settledGithubImageBuild(build)));
    if (before) return before;
    const startLimit = check === 0 && !last;
    const ended = await step.waitForEvent(`wait-github-run-${key}-${check}`, { ...run, timeout: startLimit ? `${START_WITHIN_MINUTES}m` : GITHUB_CHECK_INTERVAL });
    const found = await step.run(`check-github-build-${key}-${check}`, () => runEffect(checkGithubImageBuild(build, { ended: ended !== null, startLimit })));
    if (found.kind !== "waiting") return found;
  }
};

const BUILDERS = { servers: walkServers, github: walkGithub } satisfies Record<BuildCandidate["builder"], Builder>;

/**
 * One Image Build walks its Builders in turn: its Service's Preferred Builder, then the Build Order.
 * Each but the last has "start within" to start it, else the next gets it; the last waits. A Builder
 * that can't take it is skipped at once. A build that started never moves. Every skip lands on the
 * Image Build's trail.
 *
 *   candidates ─▶ [servers | github] ─ skipped ─▶ next ─ … ─▶ none left: failed
 *                        └─ settled (built / failed / cancelled) ─▶ done
 */
async function runImageBuild(
  build: ImageBuildTarget,
  step: EnvironmentDeploymentStepTools,
  runEffect: DeploymentInngestEffectRunner,
) {
  const candidates = await step.run(`plan-image-build-${build.serviceId}`, () => runEffect(planImageBuildWalk(build)));
  let skipped: SkipReason | null = null;
  for (const [index, candidate] of candidates.entries()) {
    const walk = { key: `${build.serviceId}-${index}`, last: index === candidates.length - 1, step, runEffect };
    const attempt = await BUILDERS[candidate.builder](build, candidate, walk);
    if (attempt.kind === "settled") return attempt.result;
    skipped = attempt.reason;
  }
  const message = skipped ? skipReasonText(skipped) : "No Builder can take this build.";
  const failed = await step.run(`fail-image-build-${build.serviceId}`, () =>
    runEffect(settleImageBuild(build, { status: "failed", message, machineId: null })));
  return failed.result;
}

export type EnvironmentDeployEventData =
  Partial<EnvironmentDeployRequestedEventData>;

export async function executeProcessEnvironmentDeploymentOnFailure(
  {
    event,
    error,
  }: {
    event: UntrustedInngestEnvelope;
    error: Error;
  },
  runEffect: DeploymentInngestEffectRunner = runInngestEffect,
) {
  const decodedEvent = decodeEnvironmentDeployFailureEnvelope(event);
  if (!decodedEvent) return;
  const environmentDeploymentId =
    decodedEvent.data.event.data.environmentDeploymentId;
  const failedRunId = decodedEvent.data.run_id;

  const context = await runEffect(
    loadDeploymentContext(environmentDeploymentId),
  );

  if (!context || isTerminalEnvironmentDeployment(context)) {
    return;
  }

  if (context.deployment.inngestRunId !== failedRunId) {
    return;
  }

  await terminalizeDeploymentFailure(
    {
      environmentDeploymentId,
      expectedInngestRunId: failedRunId,
      error: context.deployment.status === "deploying" && !deployFailureEvidence(error).failureCode
        ? new DeploymentExecutionError({
            failureCode: "sdk_deploy_outcome_unknown",
            message: "Runtime execution ended without a complete outcome; effects are unknown.",
          })
        : error,
    },
    runEffect,
  );
  // A crash mid-walk leaves the rows cancelled; their GitHub runs and grants must stop too.
  await runEffect(cancelGithubImageBuilds(failedRunId));
}

export async function executeProcessEnvironmentDeployment(
  {
    event,
    step,
    runId,
  }: {
    event: { name?: string; data: EnvironmentDeployEventData };
    step: EnvironmentDeploymentStepTools;
    runId: string;
  },
  runEffect: DeploymentInngestEffectRunner = runInngestEffect,
) {
  const environmentDeploymentId = event.data.environmentDeploymentId?.trim();
  if (!environmentDeploymentId) {
    return { environmentDeploymentId: null, skipped: true };
  }

  const claimed = await step.run("record-inngest-run", () =>
    runEffect(
      recordInngestRun({
        environmentDeploymentId,
        runId,
      }),
    ),
  );
  if (!claimed) {
    return {
      environmentDeploymentId,
      status: "owned_elsewhere",
      skipped: true,
    };
  }

  const loadedContext = deploymentContext(
    await step.run("load-deployment-context", async () => {
      const loaded = await runEffect(
        loadDeploymentContext(environmentDeploymentId),
      );

      if (!loaded) {
        throw new NonRetriableError("Environment deployment was not found.");
      }

      return loaded;
    }),
  );
  if (!loadedContext) throw new Error("Deployment context was not decoded.");
  const context: DeploymentContext = loadedContext;

  if (isTerminalEnvironmentDeployment(context)) {
    return {
      environmentDeploymentId,
      status: context.deployment.status,
      skipped: true,
    };
  }

  try {
    // Admission fan-out: every Image Build starts now, in parallel, without holding the Environment slot.
    const builds = await step.run("start-image-builds", () => runEffect(startImageBuilds(context, runId)));
    const settled = await Promise.all(builds.map((build) => runImageBuild(build, step, runEffect)));
    // Every build settles first, so the ones that finished keep their receipts for a retry.
    const unbuilt = settled.filter(({ status }) => status !== "built").map(({ image }) => image);
    if (unbuilt.length) {
      throw new DeploymentExecutionError({ failureCode: "image_build_failed", message: `Image Build failed: ${unbuilt.join(", ")}.` });
    }

    while (true) {
      const planning = await step.run(
        "mark-deployment-planning",
        () =>
          runEffect(
            beginEnvironmentDeploymentPlanning({
              environmentDeploymentId,
              expectedInngestRunId: runId,
            }),
          ),
      );
      if (planning.state === "blocked") {
        await step.sleep(
          "wait-for-active-deployment",
          DEPLOY_ADMISSION_POLL_INTERVAL,
        );
        continue;
      }
      if (planning.state === "unavailable") {
        const current = deploymentContext(
          await step.run("reload-deployment-after-planning-race", () =>
            runEffect(loadDeploymentContext(environmentDeploymentId)),
          ),
        );
        return {
          environmentDeploymentId,
          status: current?.deployment.status ?? "missing",
          skipped: true,
        };
      }
      break;
    }

    await step.run("execute-sdk-deploy", () =>
      runEffect(Effect.scoped(executeLatestEnvironmentDeployment(environmentDeploymentId, runId))),
    );
    const completed = deploymentContext(await step.run("load-deployment-outcome", () =>
      runEffect(loadDeploymentContext(environmentDeploymentId)),
    ));
    if (completed && !isTerminalEnvironmentDeployment(completed)) {
      throw new DeploymentExecutionError({ failureCode: "sdk_deploy_outcome_unknown", message: "Runtime execution ended without a durable outcome; effects are unknown." });
    }
    // The terminal row released the Environment slot; cleanup never holds it.
    if (completed?.deployment.runtimeProgress?.imageCleanup?.state === "running") {
      await step.run("clean-up-images", () =>
        runEffect(Effect.scoped(cleanUpDeploymentImages(environmentDeploymentId))),
      );
    }
    return { environmentDeploymentId, status: completed?.deployment.status ?? "missing" };

  } catch (error) {
    const latestContext = deploymentContext(
      await step.run("reload-deployment-before-failure", () =>
        runEffect(loadDeploymentContext(environmentDeploymentId)),
      ),
    );
    if (
      !latestContext ||
      isTerminalEnvironmentDeployment(latestContext)
    ) {
      return {
        environmentDeploymentId,
        status: latestContext?.deployment.status ?? "missing",
        skipped: true,
      };
    }

    const ownsFailure = await step.run("authorize-deployment-failure", () =>
      runEffect(
        ownsDeploymentRun({
          environmentDeploymentId,
          inngestRunId: runId,
        }),
      ),
    );
    if (!ownsFailure) {
      return {
        environmentDeploymentId,
        status: "owned_elsewhere",
        skipped: true,
      };
    }

    if (!isDeterministicDeployFailure(error)) {
      throw error;
    }
    return step.run("mark-deployment-failed", async () => {
      const terminalError = asNonRetriableDeployFailure(error);
      await terminalizeDeploymentFailure(
        {
          environmentDeploymentId,
          expectedInngestRunId: runId,
          error: terminalError,
        },
        runEffect,
      );
      throw terminalError;
    });
  }
}

export async function executeMarkCancelledRowBackedWorkflow(
  input: {
    event: {
      data: InngestFunctionCancelledEventData;
    };
    step: EnvironmentDeploymentStepTools;
  },
  runGithubEffect: GithubIngestionEffectRunner,
  runEffect: DeploymentInngestEffectRunner = runInngestEffect,
) {
  const functionId = input.event.data.function_id;
  const runId = input.event.data.run_id;

  if (functionId !== PROCESS_ENVIRONMENT_DEPLOYMENT_FUNCTION_ID) {
    return executeCancelGithubRowBackedWorkflow(
      { functionId, runId, step: input.step },
      runGithubEffect,
    );
  }

  const marked = await input.step.run(
    "mark-environment-deployment-cancelled",
    () => runEffect(markCancelledByInngestRunId(runId)),
  );
  // The attempt's rows are cancelled; its GitHub runs and grants must stop too.
  await input.step.run("cancel-github-builds", () => runEffect(cancelGithubImageBuilds(runId)));
  return { functionId, runId, marked };
}

export const createProcessEnvironmentDeployment = (
  inngest: PloyzInngest,
  runEffect: DeploymentInngestEffectRunner = runInngestEffect,
) =>
  inngest.createFunction(
  {
    id: PROCESS_ENVIRONMENT_DEPLOYMENT_FUNCTION_ID,
    // A lost execution may have mutated Machines. A new attempt must be explicit.
    retries: 0,
    cancelOn: [{ event: environmentDeployCancelRequestedEvent, match: "data.environmentDeploymentId" }],
    triggers: [{ event: environmentDeployRequestedEventType }],
    // Inngest counts executing steps. Keyed per Environment, an earlier attempt's deploy step would
    // stall a queued attempt's Image Builds; limited to 1 per attempt, its parallel builds would run
    // one at a time. So: per attempt, with room for its builds side by side; beyond that many, the
    // rest wait their turn. The Environment execution slot is the database's partial unique index,
    // and one run owns an attempt through its recorded run id.
    concurrency: [{ key: "event.data.environmentDeploymentId", limit: IMAGE_BUILDS_AT_ONCE }],
    onFailure: async ({ event, error }) =>
      executeProcessEnvironmentDeploymentOnFailure(
        { event, error },
        runEffect,
      ),
  },
  async ({ event, step, runId }) => {
    const decodedEvent = await step.run(
      "decode-environment-deploy-event",
      () => decodeInngestEnvelope(EnvironmentDeployRequestedEnvelope)(event),
    );
    return executeProcessEnvironmentDeployment(
      { event: decodedEvent, step, runId },
      runEffect,
    );
  },
  );

export const createMarkCancelledRowBackedWorkflow = (
  inngest: PloyzInngest,
  runEffect: DeploymentInngestEffectRunner = runInngestEffect,
) =>
  inngest.createFunction(
  {
    id: "mark-cancelled-row-backed-workflow",
    retries: 3,
    triggers: [{ event: inngestFunctionCancelledEventType }],
    concurrency: [{ key: "event.data.run_id", limit: 1 }],
  },
  async ({ event, step }) => {
    const decodedEvent = await step.run(
      "decode-row-backed-cancellation-event",
      () => decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event),
    );
    return executeMarkCancelledRowBackedWorkflow(
      { event: decodedEvent, step },
      runEffect,
      runEffect,
    );
  },
  );
