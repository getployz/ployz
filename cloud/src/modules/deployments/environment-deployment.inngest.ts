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
  executeLatestEnvironmentDeployment,
} from "#/modules/deployments/runtime-activities.server";
import { markCancelledByInngestRunId } from "#/modules/deployments/runtime-cancellation.repository.server";
import { loadDeploymentContext } from "#/modules/deployments/runtime-hydration.repository.server";
import {
  beginEnvironmentDeploymentPlanning,
  markDeploymentFailedIfOwned,
  markDeploymentStatus,
  ownsDeploymentRun,
  persistDeployApplyResult,
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
    { cause: cause instanceof Error ? cause : undefined },
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

export const PROCESS_ENVIRONMENT_DEPLOYMENT_CONCURRENCY = [
  {
    key: "event.data.environmentId",
    limit: 1,
  },
] as const;

export type EnvironmentDeploymentStepTools = Pick<
  PloyzStepTools,
  "run" | "sleep" | "sendEvent"
>;

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
    while (true) {
      const planning = await step.run(
        "mark-deployment-planning",
        () =>
          runEffect(
            beginEnvironmentDeploymentPlanning({
              environmentDeploymentId,
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

    const deploying = await step.run("mark-deployment-deploying", () =>
      runEffect(
        markDeploymentStatus({
          environmentDeploymentId,
          status: "deploying",
        }),
      ),
    );
    if (!deploying) {
      const terminalContext = deploymentContext(
        await step.run("reload-deployment-after-deploying-race", () =>
          runEffect(loadDeploymentContext(environmentDeploymentId)),
        ),
      );
      return {
        environmentDeploymentId,
        status: terminalContext?.deployment.status ?? "missing",
        skipped: true,
      };
    }

    const outcome = await step.run("execute-sdk-deploy", () =>
      runEffect(Effect.scoped(executeLatestEnvironmentDeployment(environmentDeploymentId))),
    );
    if (outcome.type === "failed") {
      const message = `Deployment stopped (${outcome.reason}): ${outcome.completed} operations completed; ${outcome.unexecuted} not attempted. The failed operation may have additional effects.`;
      if (outcome.reason === "cancelled") {
        const cancelled = await step.run("mark-runtime-deployment-cancelled", () =>
          runEffect(markCancelledByInngestRunId(runId, message)),
        );
        if (!cancelled) {
          const latest = deploymentContext(await step.run("reload-runtime-cancellation-race", () =>
            runEffect(loadDeploymentContext(environmentDeploymentId)),
          ));
          return { environmentDeploymentId, status: latest?.deployment.status ?? "missing", skipped: true };
        }
        return { environmentDeploymentId, status: "cancelled" };
      }
      throw new DeploymentExecutionError({ message, failureCode: "sdk_deploy_failed" });
    }

    const applied = await step.run("persist-deploy-apply-result", () =>
      runEffect(
        persistDeployApplyResult({
          environmentDeploymentId,
          result: { coreDeployId: null },
        }),
      ),
    );
    if (!applied) {
      const durableContext = deploymentContext(
        await step.run("reload-deployment-after-apply-race", () =>
          runEffect(loadDeploymentContext(environmentDeploymentId)),
        ),
      );
      return {
        environmentDeploymentId,
        status: durableContext?.deployment.status ?? "missing",
        skipped: true,
      };
    }

    return {
      environmentDeploymentId,
      status: "applied",
    };
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
    triggers: [{ event: environmentDeployRequestedEventType }],
    concurrency: [...PROCESS_ENVIRONMENT_DEPLOYMENT_CONCURRENCY],
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
