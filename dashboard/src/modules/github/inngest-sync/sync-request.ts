import { createGithubRepositoriesSyncRequestedEvent } from "#/modules/inngest/events";

/** Why Cloud asks for an installation's repositories to be read again. */
export type GithubSyncReason =
  | "manual-refresh"
  | "scheduled"
  | `installation.${string}`
  | `installation_repositories.${string}`;

export const githubSyncRequested = (installationId: number, reason: GithubSyncReason) =>
  createGithubRepositoriesSyncRequestedEvent({ installationId, reason });
