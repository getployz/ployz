import "@tanstack/react-start/server-only";

import { Effect } from "effect";
import { githubRepositoriesSyncRequested } from "#/modules/github/github.events";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  listCachedGithubRepositoriesForOrganization,
  listGithubInstallationsForOrganization,
} from "#/modules/github/github.repository";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { listGithubRepositoryBranches } from "./github-observation.api";
import {
  repositoryOwnerId, repositoryPaths, resolveReadableRepository, resolveReadableRepositoryById,
} from "./readable-repository.server";
import { AppConfig } from "#/server/config.server";
import { NotFound } from "#/server/public-error";

export const listGithubRepositories = Effect.fn("Github.listRepositories")(
  function* (actor: Actor, organizationSlug: string) {
    const { organization } = yield* requireAccess(actor, organizationSlug, "project:read");
    return yield* listCachedGithubRepositoriesForOrganization(organization.id);
  },
);

export const getGithubRepoAccessState = Effect.fn(
  "Github.getRepoAccessState",
)(function* (actor: Actor, organizationSlug: string) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "project:read");
  const installations = yield* listGithubInstallationsForOrganization(organization.id);
  return { hasInstallations: installations.length > 0 };
});

/** Where browser links to GitHub go: github.com, or a verify lab's fake. */
export const getGithubWeb = Effect.fn("Github.getWeb")(function* () {
  const config = yield* AppConfig;
  return { origin: config.github.origins.web };
});

export const requestGithubRepoSync = Effect.fn("Github.requestRepoSync")(
  function* (actor: Actor, organizationSlug: string) {
    const { organization } = yield* requireAccess(actor, organizationSlug, "project:edit");
    const installations = yield* listGithubInstallationsForOrganization(organization.id);
    if (installations.length === 0) {
      return { requestedInstallationCount: 0, hasInstallations: false };
    }
    yield* sendInngestEvent(
      installations.map((installation) =>
        githubRepositoriesSyncRequested.create({
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

const readableRepository = Effect.fn("Github.readableRepository")(function* (actor: Actor, input: RepositoryInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "project:read");
  const repository = yield* resolveReadableRepositoryById({ organizationId: organization.id, ...input });
  if (repository === null) {
    return yield* new NotFound({ message: "The GitHub repository installation was not found." });
  }
  return repository;
});

export const listGithubBranches = Effect.fn("Github.listBranches")(
  function* (actor: Actor, input: RepositoryInput) {
    const repository = yield* readableRepository(actor, input);
    const branches = yield* listGithubRepositoryBranches(repository.access, repository.fullName);
    return { branches };
  },
);

/**
 * The GitHub IDs of `repository` and its owner, as this Organization may read it: what an automatic deploy rule names.
 * Refused not found for a repository the Organization can't read.
 */
export const githubRepositoryOwner = Effect.fn("Github.repositoryOwner")(
  function* (actor: Actor, input: { readonly organizationSlug: string; readonly repository: string }) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "project:read");
    const readable = yield* resolveReadableRepository(organization.id, input.repository);
    if (readable === null) return yield* new NotFound({ message: `This organization can't read ${input.repository} on GitHub.` });
    return { repositoryId: readable.repositoryId, ownerId: yield* repositoryOwnerId(readable) };
  },
);

export const searchGithubFiles = Effect.fn("Github.searchFiles")(
  function* (actor: Actor, input: RepositoryInput & { readonly ref: string; readonly pattern: string }) {
    const repository = yield* readableRepository(actor, input);
    return yield* repositoryPaths(repository.access, repository.fullName, input.ref, { cap: 200, match: input.pattern, sorted: true });
  },
);
