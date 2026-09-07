import {
  drainGithubCheckSuiteTransitionOutbox,
  drainGithubEnvironmentTriggerOutbox,
  type GithubIngestionStepTools,
} from "#/modules/github/inngest-ingestion/outbox";
import type { GithubIngestionEffectRunner } from "#/modules/github/inngest-ingestion/process";
import type { PloyzInngest } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";

export async function executeSweepGithubIngestionOutboxes(
  step: GithubIngestionStepTools,
  runEffect: GithubIngestionEffectRunner,
) {
  const environmentTriggers = await drainGithubEnvironmentTriggerOutbox(
    step,
    "sweep",
    runEffect,
  );
  const checkSuiteTransitions = await drainGithubCheckSuiteTransitionOutbox(
    step,
    "sweep",
    runEffect,
  );
  return { environmentTriggers, checkSuiteTransitions };
}

export const createSweepGithubIngestionOutboxes = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "sweep-github-ingestion-outboxes",
    retries: 5,
    triggers: [{ cron: "*/5 * * * *" }],
    concurrency: [{ limit: 1 }],
  },
  async ({ step }) =>
    executeSweepGithubIngestionOutboxes(
      step,
      runInngestEffect,
    ),
  );
