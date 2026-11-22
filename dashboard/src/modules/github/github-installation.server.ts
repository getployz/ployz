import "@tanstack/react-start/server-only";

import { Effect } from "effect";
import type {
  GithubInstallationRepositoriesWebhookEventData,
  GithubInstallationWebhookEventData,
} from "#/modules/inngest/events";
import {
  bumpGithubRepositoryCacheVersion,
  deleteCachedGithubRepositoriesById,
  deleteCachedGithubRepositoriesForInstallation,
  deleteGithubInstallation,
  listLinkedOrganizationIds,
  lockGithubRepositoryCache,
  suspendGithubInstallation,
  upsertGithubInstallation,
} from "#/modules/github/github.repository";
import { Database } from "#/server/database.server";

function isUsableInstallationAction(
  action: GithubInstallationWebhookEventData["action"],
) {
  return (
    action === "created" ||
    action === "unsuspend" ||
    action === "new_permissions_accepted"
  );
}

/**
 * An installation webhook records what GitHub told us and grants nothing: only an Organization's link row does.
 * `suspend` marks the installation suspended and empties its repository cache, keeping every Organization's link, so
 * `unsuspend` brings it back; `deleted` forgets the installation, which cascades to its cache and every link. Both
 * name the Organizations that were linked, whose builds through it must stop now.
 */
export const processGithubInstallationEvent = Effect.fn(
  "Github.processInstallationEvent",
)(function* (payload: GithubInstallationWebhookEventData) {
  if (isUsableInstallationAction(payload.action)) {
    yield* upsertGithubInstallation({
      installationId: payload.installation.id,
      accountLogin: payload.installation.account.login,
      accountType: payload.installation.account.type,
      accountAvatarUrl: payload.installation.account.avatar_url,
    });
    return {
      installationId: payload.installation.id,
      shouldSyncRepositories: true,
      stopBuildsFor: [],
    };
  }

  const stopBuildsFor = yield* listLinkedOrganizationIds(payload.installation.id);
  if (payload.action === "suspend") {
    yield* (yield* Database).transaction(Effect.gen(function* () {
      yield* lockGithubRepositoryCache(payload.installation.id);
      yield* suspendGithubInstallation(payload.installation.id);
      yield* deleteCachedGithubRepositoriesForInstallation(payload.installation.id);
      yield* bumpGithubRepositoryCacheVersion(payload.installation.id);
    }));
  } else yield* deleteGithubInstallation(payload.installation.id);
  return {
    installationId: payload.installation.id,
    shouldSyncRepositories: false,
    stopBuildsFor,
  };
});

/**
 * A removed repository leaves the cache now. An added one waits for the sync this asks for, because GitHub's payload
 * names it without the default branch, URL and timestamp a cache entry holds. The version bump makes a sync already
 * listing list again, since the request for a new one is skipped while it runs.
 */
export const processGithubInstallationRepositoriesEvent = Effect.fn(
  "Github.processInstallationRepositoriesEvent",
)(function* (payload: GithubInstallationRepositoriesWebhookEventData) {
  const removedRepositoryIds = payload.repositories_removed.map(
    (repository) => repository.id,
  );
  yield* (yield* Database).transaction(Effect.gen(function* () {
    yield* lockGithubRepositoryCache(payload.installation.id);
    if (removedRepositoryIds.length > 0) {
      yield* deleteCachedGithubRepositoriesById(
        payload.installation.id,
        removedRepositoryIds,
      );
    }
    yield* bumpGithubRepositoryCacheVersion(payload.installation.id);
  }));
  return {
    installationId: payload.installation.id,
    shouldSyncRepositories: payload.repositories_added.length > 0,
  };
});
