import "@tanstack/react-start/server-only";

import { Effect, Schema } from "effect";
import { Minimatch } from "minimatch";
import {
  createGithubRepositoriesSyncRequestedEvent,
  type GithubInstallationRepositoriesWebhookEventData,
  type GithubInstallationWebhookEventData,
} from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { listGithubRepositoryFiles, listGithubRepositoryBranches } from "#/modules/github/github.api";
import {
  deleteCachedGithubRepositoriesById,
  deleteGithubInstallation,
  getCachedGithubRepositoryForOrganization,
  listCachedGithubRepositoriesForOrganization,
  listGithubInstallationsForOrganization,
  upsertCachedGithubRepositories,
  upsertGithubInstallation,
} from "#/modules/github/github.repository";
import type {
  GithubRepository,
} from "#/modules/github/github";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { accessFor, type GithubAccess } from "./github-access";
import { GithubApi, resolveGithubRepository } from "./github-observation.api";
import { githubIdSchema, githubRepositoryFullNameSchema } from "./github-ingestion.contracts";
import { normalizePublicGithubRepository } from "./public-repository";
import { AppConfig, type GithubOrigins } from "#/server/config.server";
import { Validation, NotFound } from "#/server/public-error";

export function getGithubAppInstallUrl(github: { readonly appSlug: string; readonly origins: GithubOrigins }) {
  return `${github.origins.web}/apps/${github.appSlug}/installations/new`;
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

/**
 * An installation webhook records what GitHub told us and grants nothing: only an Organization's link row does.
 * `deleted` and `suspend` forget the installation, which cascades to its cache and every Organization's link.
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
    };
  }

  yield* deleteGithubInstallation(payload.installation.id);
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
  if (addedRepositories.length > 0) {
    yield* upsertCachedGithubRepositories({
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

/** The GitHub repositories the Organization's linked installations grant, which its repository picker lists. */
export const listGithubRepositories = Effect.fn("Github.listRepositories")(
  function* (actor: Actor, organizationSlug: string) {
    const { organization } = yield* requireAccess(actor, organizationSlug, "operate");
    return yield* listCachedGithubRepositoriesForOrganization(organization.id);
  },
);

export const getGithubRepoAccessState = Effect.fn(
  "Github.getRepoAccessState",
)(function* (actor: Actor, organizationSlug: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "operate");
  const installations = yield* listGithubInstallationsForOrganization(organization.id);
  return { hasInstallations: installations.length > 0 };
});

/** Where browser links to GitHub go: github.com, or a verify lab's fake. */
export const getGithubWeb = Effect.fn("Github.getWeb")(function* () {
  const config = yield* AppConfig;
  return { origin: config.github.origins.web, installUrl: getGithubAppInstallUrl(config.github) };
});

export const requestGithubRepoSync = Effect.fn("Github.requestRepoSync")(
  function* (actor: Actor, organizationSlug: string) {
    const { organization } = yield* requireAccess(actor, organizationSlug, "operate");
    const installations = yield* listGithubInstallationsForOrganization(organization.id);
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

type RepositoryInput = {
  readonly organizationSlug: string;
  readonly repositoryId: number;
  readonly installationId: number | null;
};

/** A repository as the Organization reads it: through one of its linked installations, or publicly. */
const readableRepository = Effect.fn("Github.readableRepository")(function* (actor: Actor, input: RepositoryInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const access: GithubAccess = accessFor(organization.id, input.installationId);
  const repository = input.installationId === null
    ? yield* resolveGithubRepository(access, input.repositoryId)
    : yield* getCachedGithubRepositoryForOrganization({
      organizationId: organization.id, installationId: input.installationId, repositoryId: input.repositoryId,
    });
  if (repository === null) {
    return yield* new NotFound({ message: "The GitHub repository installation was not found." });
  }
  return { access, fullName: repository.fullName };
});

export const listGithubBranches = Effect.fn("Github.listBranches")(
  function* (actor: Actor, input: RepositoryInput) {
    const repository = yield* readableRepository(actor, input);
    const branches = yield* listGithubRepositoryBranches(repository.access, repository.fullName);
    return { branches };
  },
);

export { verifyWebhookSignature } from "#/modules/github/github.api";

export const searchGithubFiles = Effect.fn("Github.searchFiles")(
  function* (actor: Actor, input: RepositoryInput & { readonly ref: string; readonly pattern: string }) {
    const repository = yield* readableRepository(actor, input);
    const files = yield* listGithubRepositoryFiles(repository.access, repository.fullName, input.ref);
    const matcher = basicGlob(input.pattern);
    const paths = files.paths.filter((path) => matcher.match(path)).sort();
    return {
      paths: paths.slice(0, 200),
      truncated: files.truncated || paths.length > 200,
    };
  },
);

/**
 * A user-supplied glob over repository paths: `*` and `?` within a segment, `**` across them, dotfiles included. Basic
 * globs only, so a pattern can't brace-expand without bound.
 */
export const basicGlob = (pattern: string) =>
  new Minimatch(pattern, { dot: true, nobrace: true, noext: true, nonegate: true, nocomment: true });

export const resolvePublicGithubRepository = Effect.fn("Github.resolvePublicRepository")(function* (input: string) {
  const name = normalizePublicGithubRepository(input);
  if (!name) return yield* new Validation({ message: "Enter owner/repo or a GitHub repository URL. Select the branch separately." });
  const api = yield* GithubApi;
  const repo = yield* api.json({ access: { type: "public" }, path: `/repos/${name}`, operation: "resolve_repository",
    schema: Schema.Struct({ id: githubIdSchema, full_name: githubRepositoryFullNameSchema, private: Schema.Literal(false), default_branch: Schema.NonEmptyString }) });
  return { fullName: repo.full_name, repositoryId: repo.id, access: { type: "public" as const }, defaultBranch: repo.default_branch };
});
