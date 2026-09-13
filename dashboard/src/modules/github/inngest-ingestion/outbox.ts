import {
  createGithubCheckSuiteTransitionEvent,
  createGithubEnvironmentTriggerPersistedEvent,
} from "#/modules/inngest/events";
import type { PloyzStepTools } from "#/modules/inngest/client";
import type { GithubIngestionEffectRunner } from "#/modules/github/inngest-ingestion/process";
import {
  acknowledgeGithubCheckSuiteTransition,
  acknowledgeGithubEnvironmentTrigger,
  listPendingGithubCheckSuiteTransitions,
  listPendingGithubEnvironmentTriggers,
  loadPendingGithubCheckSuiteTransition,
  type GithubPendingCheckSuiteTransition,
} from "#/modules/github/github-ingestion.repository";

type DurableGithubPendingCheckSuiteTransition = Omit<
  GithubPendingCheckSuiteTransition,
  "sourceUpdatedAt"
> & { sourceUpdatedAt: Date | string };

export type GithubIngestionStepTools = Pick<
  PloyzStepTools,
  "run" | "sendEvent"
>;

export async function drainGithubEnvironmentTriggerOutbox(
  step: GithubIngestionStepTools,
  stepPrefix: string,
  runEffect: GithubIngestionEffectRunner,
) {
  const pending = await step.run(`${stepPrefix}-list-environment-outbox`, () =>
    runEffect(listPendingGithubEnvironmentTriggers({ limit: 100 })),
  );
  for (const trigger of pending) {
    await step.sendEvent(
      `publish-environment-trigger-${trigger.triggerId}-revision-${trigger.triggerRevision}`,
      createGithubEnvironmentTriggerPersistedEvent({
        triggerId: trigger.triggerId,
        triggerRevision: trigger.triggerRevision,
        installationId: trigger.installationId,
        repositoryId: trigger.repositoryId,
        ref: trigger.ref,
        headSha: trigger.headSha,
        environmentId: trigger.environmentId,
        serviceIds: trigger.serviceIds,
        selection: trigger.selection,
        sourceDeliveryId: trigger.sourceDeliveryId,
        sourceReceiptSequence: trigger.sourceReceiptSequence,
      }),
    );
    await step.run(
      `ack-environment-trigger-${trigger.triggerId}-revision-${trigger.triggerRevision}`,
      () =>
        runEffect(
          acknowledgeGithubEnvironmentTrigger({
            triggerId: trigger.triggerId,
            triggerRevision: trigger.triggerRevision,
            publishedAt: new Date(),
          }),
        ),
    );
  }
  return pending.length;
}

export async function drainGithubCheckSuiteTransitionOutbox(
  step: GithubIngestionStepTools,
  stepPrefix: string,
  runEffect: GithubIngestionEffectRunner,
) {
  const pending = await step.run(`${stepPrefix}-list-check-suite-outbox`, () =>
    runEffect(listPendingGithubCheckSuiteTransitions({ limit: 100 })),
  );
  for (const transition of pending) {
    await publishGithubCheckSuiteTransition(step, transition, runEffect);
  }
  return pending.length;
}

async function publishGithubCheckSuiteTransition(
  step: GithubIngestionStepTools,
  transition: DurableGithubPendingCheckSuiteTransition,
  runEffect: GithubIngestionEffectRunner,
) {
  await step.sendEvent(
    `publish-check-suite-${transition.installationId}-${transition.repositoryId}-${transition.checkSuiteId}-revision-${transition.transitionRevision}`,
    createGithubCheckSuiteTransitionEvent({
      ...transition,
      sourceUpdatedAt: new Date(transition.sourceUpdatedAt).toISOString(),
    }),
  );
  return step.run(
    `ack-check-suite-${transition.installationId}-${transition.repositoryId}-${transition.checkSuiteId}-revision-${transition.transitionRevision}`,
    () =>
      runEffect(
        acknowledgeGithubCheckSuiteTransition({
          installationId: transition.installationId,
          repositoryId: transition.repositoryId,
          checkSuiteId: transition.checkSuiteId,
          transitionRevision: transition.transitionRevision,
        }),
      ),
  );
}

export async function publishPendingGithubCheckSuiteTransition(
  step: GithubIngestionStepTools,
  stepPrefix: string,
  identity: {
    installationId: number;
    repositoryId: number;
    checkSuiteId: number;
  },
  runEffect: GithubIngestionEffectRunner,
) {
  const transition = await step.run(
    `${stepPrefix}-load-check-suite-outbox`,
    () => runEffect(loadPendingGithubCheckSuiteTransition(identity)),
  );
  if (!transition) return { published: false, acknowledged: false };
  const acknowledgment = await publishGithubCheckSuiteTransition(
    step,
    transition,
    runEffect,
  );
  return {
    published: true,
    acknowledged: acknowledgment.acknowledged,
  };
}
