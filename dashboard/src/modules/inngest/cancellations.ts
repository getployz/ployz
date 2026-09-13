import {
  PROCESS_GITHUB_CHECK_SUITE_RECEIVED_FUNCTION_ID,
  PROCESS_GITHUB_PUSH_RECEIVED_FUNCTION_ID,
} from "#/modules/inngest/row-backed-workflow-ids";
import type { GithubIngestionEffectRunner } from "#/modules/github/inngest-ingestion/process";
import { cancelGithubDelivery } from "#/modules/github/github-ingestion.repository";
import type { PloyzStepTools } from "#/modules/inngest/client";

export type GithubCancellationTools = Pick<PloyzStepTools, "run">;

export async function executeCancelGithubRowBackedWorkflow(
  input: {
    functionId: string;
    runId: string;
    step: GithubCancellationTools;
  },
  runEffect: GithubIngestionEffectRunner,
) {
  if (
    input.functionId === PROCESS_GITHUB_PUSH_RECEIVED_FUNCTION_ID ||
    input.functionId === PROCESS_GITHUB_CHECK_SUITE_RECEIVED_FUNCTION_ID
  ) {
    const marked = await input.step.run("cancel-github-delivery", async () => {
      const cancelled = await runEffect(
        cancelGithubDelivery({ processingRunId: input.runId }),
      );
      return cancelled.disposition === "terminal";
    });

    return {
      functionId: input.functionId,
      runId: input.runId,
      marked,
    };
  }

  return {
    functionId: input.functionId,
    runId: input.runId,
    skipped: true,
  };
}
