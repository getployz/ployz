import "@tanstack/react-start/server-only";

import { Data, Effect, Schema } from "effect";
import { Minimatch } from "minimatch";
import {
  createGithubRepositoriesSyncRequestedEvent,
  type GithubInstallationRepositoriesWebhookEventData,
  type GithubInstallationWebhookEventData,
} from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { listGithubRepositoryFiles, listGithubRepositoryBranches } from "#/modules/github/github.api";
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
  GithubRepository,
} from "#/modules/github/github";
import type { Actor } from "#/modules/identity/actor";
import { GithubApi, resolveGithubRepository } from "./github-observation.api";
import { githubIdSchema, githubRepositoryFullNameSchema } from "./github-ingestion.contracts";
import { normalizePublicGithubRepository } from "./public-repository";
import { AppConfig } from "#/server/config.server";
import { Validation, NotFound } from "#/server/public-error";

export class GithubAccountNotLinked extends Data.TaggedError(
  "GithubAccountNotLinked",
)<{ readonly accountId: string }> {
  readonly publicErrorCategory = "internal" as const;
  readonly retriable = false as const;
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
  const installations = yield* listGithubInstallationsForUser(actor.userId);
  return { hasInstallations: installations.length > 0 };
});

export const getGithubInstallUrl = Effect.fn("Github.getInstallUrl")(
  function* () {
    const config = yield* AppConfig;
    return { url: getGithubAppInstallUrl({ slug: config.github.appSlug }) };
  },
);

export const requestGithubRepoSync = Effect.fn("Github.requestRepoSync")(
  function* (actor: Actor) {
    const installations = yield* listGithubInstallationsForUser(actor.userId);
    if (installations.length === 0) {
      return { requestedInstallationCount: 0, hasInstallations: false };
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
    };
  },
);

export const listGithubBranches = Effect.fn("Github.listBranches")(
  function* (
    actor: Actor,
    input: { readonly repositoryId: number; readonly installationId: number | null },
  ) {
    const repository = input.installationId === null
      ? yield* resolveGithubRepository(null, input.repositoryId)
      : yield* getCachedGithubRepositoryForUser({ userId: actor.userId, repositoryId: input.repositoryId, installationId: input.installationId });
    if (repository === null) {
      return yield* new NotFound({
        message: "The GitHub repository installation was not found.",
      });
    }
    const branches = yield* listGithubRepositoryBranches(
      input.installationId,
      repository.fullName,
    );
    return { branches };
  },
);

export { verifyWebhookSignature } from "#/modules/github/github.api";

export const searchGithubFiles = Effect.fn("Github.searchFiles")(
  function* (actor: Actor, input: {
    repositoryId: number;
    installationId: number | null;
    ref: string;
    pattern: string;
  }) {
    const repository = input.installationId === null
      ? yield* resolveGithubRepository(null, input.repositoryId)
      : yield* getCachedGithubRepositoryForUser({ userId: actor.userId, repositoryId: input.repositoryId, installationId: input.installationId });
    if (repository === null) {
      return yield* new NotFound({ message: "The GitHub repository installation was not found." });
    }
    const files = yield* listGithubRepositoryFiles(input.installationId, repository.fullName, input.ref);
    // Basic globs only: avoid unbounded brace expansion for user-supplied patterns.
    const matcher = new Minimatch(input.pattern, {
      dot: true, nobrace: true, noext: true, nonegate: true, nocomment: true,
    });
    const paths = files.paths.filter((path) => matcher.match(path)).sort();
    return {
      paths: paths.slice(0, 200),
      truncated: files.truncated || paths.length > 200,
    };
  },
);

export const resolvePublicGithubRepository = Effect.fn("Github.resolvePublicRepository")(function* (input: string) {
  const name = normalizePublicGithubRepository(input);
  if (!name) return yield* new Validation({ message: "Enter owner/repo or a GitHub repository URL. Select the branch separately." });
  const api = yield* GithubApi;
  const repo = yield* api.json({ installationId: null, url: `https://api.github.com/repos/${name}`, operation: "resolve_repository",
    schema: Schema.Struct({ id: githubIdSchema, full_name: githubRepositoryFullNameSchema, private: Schema.Literal(false), default_branch: Schema.NonEmptyString }) });
  return { fullName: repo.full_name, repositoryId: repo.id, access: { type: "public" as const }, defaultBranch: repo.default_branch };
});
