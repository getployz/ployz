import { Schema } from "effect";
import {
  githubCheckSuiteKey,
  githubCheckSuiteReceivedEventDataSchema,
  githubIdSchema,
  githubPullRequestKey,
  githubPullRequestReceivedEventDataSchema,
  githubPushBranchKey,
  githubPushReceivedEventDataSchema,
  type GithubCheckSuiteReceivedEventInput,
  type GithubPullRequestReceivedEventInput,
  type GithubPushReceivedEventInput,
} from "#/modules/github/github-ingestion.contracts";
import {
  githubInstallationRepositoriesWebhookEventDataSchema,
  githubInstallationWebhookEventDataSchema,
} from "#/modules/github/github-webhook-contracts";
import { defineEvent } from "#/modules/inngest/define-event";

/** Each delivery is keyed by GitHub's delivery id, so a redelivery starts nothing new. */
export const githubInstallationReceived = defineEvent({
  name: "github/installation.received",
  schema: githubInstallationWebhookEventDataSchema,
  id: ({ deliveryId }) => deliveryId,
});

export const githubInstallationRepositoriesReceived = defineEvent({
  name: "github/installation-repositories.received",
  schema: githubInstallationRepositoriesWebhookEventDataSchema,
  id: ({ deliveryId }) => deliveryId,
});

/** What asked for the sync: a webhook (named by its event and action), the connect flow, or the nightly schedule. */
const RepositoriesSyncReason = Schema.Literals([
  "installation.created",
  "installation.deleted",
  "installation.suspend",
  "installation.unsuspend",
  "installation.new_permissions_accepted",
  "installation_repositories.added",
  "installation_repositories.removed",
  "manual-refresh",
  "scheduled",
]);

export const githubRepositoriesSyncRequested = defineEvent({
  name: "github/repositories-sync.requested",
  schema: Schema.Struct({ installationId: githubIdSchema, reason: RepositoriesSyncReason }),
});

export const githubPushReceived = defineEvent({
  name: "github/push.received",
  schema: githubPushReceivedEventDataSchema,
  id: ({ deliveryId }) => deliveryId,
});

export const githubCheckSuiteReceived = defineEvent({
  name: "github/check-suite.received",
  schema: githubCheckSuiteReceivedEventDataSchema,
  id: ({ deliveryId }) => deliveryId,
});

export const githubPullRequestReceived = defineEvent({
  name: "github/pull-request.received",
  schema: githubPullRequestReceivedEventDataSchema,
  id: ({ deliveryId }) => deliveryId,
});

/** A delivery's event, its authority key derived and the data decoded to the canonical form; throws for data that isn't. */
export const createGithubPushReceivedEvent = (input: GithubPushReceivedEventInput) =>
  githubPushReceived.create(
    Schema.decodeUnknownSync(githubPushReceived.schema)(
      { ...input, branchKey: githubPushBranchKey(input) },
      { onExcessProperty: "error" },
    ),
  );

export const createGithubCheckSuiteReceivedEvent = (input: GithubCheckSuiteReceivedEventInput) =>
  githubCheckSuiteReceived.create(
    Schema.decodeUnknownSync(githubCheckSuiteReceived.schema)(
      { ...input, checkSuiteKey: githubCheckSuiteKey(input) },
      { onExcessProperty: "error" },
    ),
  );

export const createGithubPullRequestReceivedEvent = (input: GithubPullRequestReceivedEventInput) =>
  githubPullRequestReceived.create(
    Schema.decodeUnknownSync(githubPullRequestReceived.schema)(
      { ...input, pullRequestKey: githubPullRequestKey(input) },
      { onExcessProperty: "error" },
    ),
  );

/**
 * A dispatched GitHub build run completed, whatever its conclusion, or its runner's final report
 * settled the build. The waiting Image Build checks it. `id` dedupes the send.
 */
export const githubBuildRunCompleted = defineEvent({
  name: "github/build-run.completed",
  schema: Schema.Struct({ runId: Schema.Number }),
});

export const createGithubBuildRunCompletedEvent = ({ id, runId }: { id: string; runId: number }) =>
  ({ id, ...githubBuildRunCompleted.create({ runId }) }) as const;

export type GithubInstallationWebhookEventData = typeof githubInstallationReceived.schema.Type;
export type GithubInstallationRepositoriesWebhookEventData = typeof githubInstallationRepositoriesReceived.schema.Type;
