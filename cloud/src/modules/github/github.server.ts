import "@tanstack/react-start/server-only";

import { Data, Effect } from "effect";
import {
  createGithubRepositoriesSyncRequestedEvent,
  type GithubInstallationRepositoriesWebhookEventData,
  type GithubInstallationWebhookEventData,
} from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { listInstallationRepoBranches } from "#/modules/github/github.api";
import {
  deleteCachedGithubRepositories,
  deleteCachedGithubRepositoriesById,
  deleteGithubInstallationsByProviderId,
  findUserIdByGithubAccountId,
  getCachedGithubRepositoryForUser,
  listGithubInstallationsForUser,
  listGithubInstallationUserIds,
  upsertCachedGithubRepositories,
  upsertGithubInstallation,
} from "#/modules/github/github.repository";
import type {
  GithubBranch,
  GithubRepository,
} from "#/modules/github/github";
import type { Actor } from "#/modules/identity/actor";
import { AppConfig } from "#/server/config.server";
import { NotFound } from "#/server/public-error";

export class GithubAccountNotLinked extends Data.TaggedError(
  "GithubAccountNotLinked",
)<{ readonly accountId: string }> {
  readonly publicErrorCategory = "internal" as const;
  readonly retriable = false as const;
}

function isGithubAppConfigured(config: {
  readonly github: {
    readonly appId: string | undefined;
    readonly appPrivateKey: unknown | undefined;
    readonly appSlug: string | undefined;
  };
}) {
  return config.github.appId !== undefined &&
    config.github.appPrivateKey !== undefined &&
    config.github.appSlug !== undefined;
}

export function getGithubAppInstallUrl(input: { readonly slug: string }) {
  return `https://github.com/apps/${input.slug}/installations/new`;
}

function isUsableInstallationAction(
  action: GithubInstallationWebhookEventData["action"],
) {
  return (
    action === "created" ||
    action === "unsuspend" ||
    action === "new_permissions_accepted"
  );
}

const getGithubWebhookAffectedUserIds = Effect.fn(
  "Github.getWebhookAffectedUserIds",
)(function* (payload: GithubInstallationRepositoriesWebhookEventData) {
  const [installationUserIds, senderUserId] = yield* Effect.all([
    listGithubInstallationUserIds(payload.installation.id),
    findUserIdByGithubAccountId(String(payload.sender.id)),
  ]);
  return Array.from(
    new Set(
      senderUserId === null
        ? installationUserIds
        : [...installationUserIds, senderUserId],
    ),
  );
});

function githubRepositoryFromWebhook(
  repository: GithubInstallationRepositoriesWebhookEventData["repositories_added"][number],
): GithubRepository {
  return {
    id: repository.id,
    name: repository.name,
    full_name: repository.full_name,
    private: repository.private,
    default_branch: repository.default_branch,
    html_url: repository.html_url,
    repo_updated_at: repository.updated_at,
  };
}

export const processGithubInstallationEvent = Effect.fn(
  "Github.processInstallationEvent",
)(function* (payload: GithubInstallationWebhookEventData) {
  if (isUsableInstallationAction(payload.action)) {
    const senderId = String(payload.sender.id);
    const userId = yield* findUserIdByGithubAccountId(senderId);
    if (userId === null) {
      yield* Effect.logError("GitHub installation sender is not linked", {
        deliveryId: payload.deliveryId,
        senderId,
        senderLogin: payload.sender.login,
        installationId: payload.installation.id,
      });
      return yield* new GithubAccountNotLinked({ accountId: senderId });
    }
    yield* upsertGithubInstallation({
      userId,
      installationId: payload.installation.id,
      accountLogin: payload.installation.account.login,
      accountType: payload.installation.account.type,
      accountAvatarUrl: payload.installation.account.avatar_url,
    });
    return {
      installationId: payload.installation.id,
      shouldSyncRepositories: true,
    };
  }

  yield* Effect.all([
    deleteGithubInstallationsByProviderId(payload.installation.id),
    deleteCachedGithubRepositories(payload.installation.id),
  ], { concurrency: "unbounded" });
  return {
    installationId: payload.installation.id,
    shouldSyncRepositories: false,
  };
});

export const processGithubInstallationRepositoriesEvent = Effect.fn(
  "Github.processInstallationRepositoriesEvent",
)(function* (payload: GithubInstallationRepositoriesWebhookEventData) {
  const addedRepositories = payload.repositories_added.map(
    githubRepositoryFromWebhook,
  );
  const removedRepositoryIds = payload.repositories_removed.map(
    (repository) => repository.id,
  );
  if (addedRepositories.length === 0 && removedRepositoryIds.length === 0) {
    return {
      installationId: payload.installation.id,
      shouldSyncRepositories: false,
    };
  }

  const userIds = yield* getGithubWebhookAffectedUserIds(payload);
  if (addedRepositories.length > 0) {
    yield* upsertCachedGithubRepositories({
      userIds,
      installationId: payload.installation.id,
      repositories: addedRepositories,
      syncedAt: new Date(),
    });
  }
  if (removedRepositoryIds.length > 0) {
    yield* deleteCachedGithubRepositoriesById(
      payload.installation.id,
      removedRepositoryIds,
    );
  }
  return {
    installationId: payload.installation.id,
    shouldSyncRepositories: false,
  };
});

export const getGithubRepoAccessState = Effect.fn(
  "Github.getRepoAccessState",
)(function* (actor: Actor) {
  const config = yield* AppConfig;
  if (!isGithubAppConfigured(config)) {
    return { hasInstallations: false, configured: false as const };
  }
  const installations = yield* listGithubInstallationsForUser(actor.userId);
  return {
    hasInstallations: installations.length > 0,
    configured: true as const,
  };
});

export const getGithubInstallUrl = Effect.fn("Github.getInstallUrl")(
  function* () {
    const config = yield* AppConfig;
    const appSlug = config.github.appSlug;
    if (!isGithubAppConfigured(config) || appSlug === undefined) {
      return { url: null, configured: false as const };
    }
    const url = getGithubAppInstallUrl({ slug: appSlug });
    return { url, configured: true as const };
  },
);

export const requestGithubRepoSync = Effect.fn("Github.requestRepoSync")(
  function* (actor: Actor) {
    const config = yield* AppConfig;
    if (!isGithubAppConfigured(config)) {
      return {
        requestedInstallationCount: 0,
        hasInstallations: false,
        configured: false as const,
      };
    }
    const installations = yield* listGithubInstallationsForUser(actor.userId);
    if (installations.length === 0) {
      return {
        requestedInstallationCount: 0,
        hasInstallations: false,
        configured: true as const,
      };
    }
    yield* sendInngestEvent(
      installations.map((installation) =>
        createGithubRepositoriesSyncRequestedEvent({
          installationId: installation.installationId,
          reason: "manual-refresh",
        }),
      ),
    );
    return {
      requestedInstallationCount: installations.length,
      hasInstallations: true,
      configured: true as const,
    };
  },
);

export const listGithubBranches = Effect.fn("Github.listBranches")(
  function* (
    actor: Actor,
    input: { readonly repositoryId: number; readonly installationId: number },
  ) {
    const config = yield* AppConfig;
    if (!isGithubAppConfigured(config)) {
      const branches: GithubBranch[] = [];
      return {
        branches,
        hasInstallations: false,
        configured: false as const,
      };
    }
    const repository = yield* getCachedGithubRepositoryForUser({
      userId: actor.userId,
      ...input,
    });
    if (repository === null) {
      return yield* new NotFound({
        message: "The GitHub repository installation was not found.",
      });
    }
    const branches = yield* listInstallationRepoBranches(
      input.installationId,
      repository.fullName,
    );
    return {
      branches,
      hasInstallations: true,
      configured: true as const,
    };
  },
);

export { verifyWebhookSignature } from "#/modules/github/github.api";
