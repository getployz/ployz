import {
  createGithubRepositoriesSyncRequestedEvent,
  githubInstallationReceivedEvent,
  githubInstallationReceivedEventType,
  githubInstallationRepositoriesReceivedEvent,
  githubInstallationRepositoriesReceivedEventType,
  inngestEventEnvelopeFields,
  type GithubInstallationWebhookEventData,
  type GithubInstallationRepositoriesWebhookEventData,
} from "#/modules/inngest/events";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { Schema } from "effect";
import type { GithubSyncEffectRunner } from "#/modules/github/inngest-sync/sync";
import {
  processGithubInstallationEvent,
  processGithubInstallationRepositoriesEvent,
} from "#/modules/github/github.server";
import {
  githubInstallationRepositoriesWebhookEventDataSchema,
  githubInstallationWebhookEventDataSchema,
} from "#/modules/github/github-webhook-contracts";
import { runInngestEffect } from "#/server/run.server";

export type GithubInstallationWebhookStepTools = Pick<
  PloyzStepTools,
  "run" | "sendEvent"
>;

const GithubInstallationEventEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(githubInstallationReceivedEvent),
  data: githubInstallationWebhookEventDataSchema,
});
const GithubInstallationRepositoriesEventEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(githubInstallationRepositoriesReceivedEvent),
  data: githubInstallationRepositoriesWebhookEventDataSchema,
});

export async function executeProcessGithubInstallationReceived(
  {
    event,
    step,
  }: {
    event: unknown;
    step: GithubInstallationWebhookStepTools;
  },
  runEffect: GithubSyncEffectRunner,
) {
  const payload = await step.run("decode-installation-event", () =>
    decodeInngestEnvelope(GithubInstallationEventEnvelope)(event)
      .data satisfies GithubInstallationWebhookEventData,
  );
  const result = await step.run("process-installation-event", () =>
    runEffect(processGithubInstallationEvent(payload)),
  );

  if (result.shouldSyncRepositories) {
    await step.sendEvent(
      "request-repository-sync",
      createGithubRepositoriesSyncRequestedEvent({
        installationId: result.installationId,
        reason: `installation.${payload.action}`,
      }),
    );
  }

  return result;
}

export async function executeProcessGithubInstallationRepositoriesReceived(
  {
    event,
    step,
  }: {
    event: unknown;
    step: GithubInstallationWebhookStepTools;
  },
  runEffect: GithubSyncEffectRunner,
) {
  const payload = await step.run(
    "decode-installation-repositories-event",
    () =>
      decodeInngestEnvelope(GithubInstallationRepositoriesEventEnvelope)(event)
        .data satisfies GithubInstallationRepositoriesWebhookEventData,
  );
  const result = await step.run(
    "process-installation-repositories-event",
    () => runEffect(processGithubInstallationRepositoriesEvent(payload)),
  );

  if (result.shouldSyncRepositories) {
    await step.sendEvent(
      "request-repository-sync",
      createGithubRepositoriesSyncRequestedEvent({
        installationId: result.installationId,
        reason: `installation_repositories.${payload.action}`,
      }),
    );
  }

  return result;
}

export const createProcessGithubInstallationReceived = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "process-github-installation-received",
    retries: 3,
    triggers: [{ event: githubInstallationReceivedEventType }],
    concurrency: [{ key: "event.data.installation.id", limit: 1 }],
  },
  async ({ event, step }) =>
    executeProcessGithubInstallationReceived(
      { event, step },
      runInngestEffect,
    ),
  );

export const createProcessGithubInstallationRepositoriesReceived = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "process-github-installation-repositories-received",
      retries: 3,
      triggers: [{ event: githubInstallationRepositoriesReceivedEventType }],
      concurrency: [{ key: "event.data.installation.id", limit: 1 }],
    },
    async ({ event, step }) =>
      executeProcessGithubInstallationRepositoriesReceived(
        { event, step },
        runInngestEffect,
      ),
  );
