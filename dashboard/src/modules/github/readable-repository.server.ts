import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { GithubApi, isGithubObservationNotFound, resolveGithubBranchHead } from "./github-observation.api";
import { githubIdSchema, githubRepositoryFullNameSchema } from "./github-ingestion.contracts";
import { findGithubRepositoryByNameForOrganization } from "./github.repository";
import { normalizePublicGithubRepository } from "./public-repository";

/** A repository the Organization may read, and how: through a member's installation, or publicly (`installationId` null). */
export type ReadableRepository = {
  readonly fullName: string;
  readonly repositoryId: number;
  readonly installationId: number | null;
  readonly defaultBranch: string;
};

const PublicRepository = Schema.Struct({
  id: githubIdSchema,
  full_name: githubRepositoryFullNameSchema,
  private: Schema.Boolean,
  default_branch: Schema.NonEmptyString,
});

/**
 * `owner/name` as this Organization may read it: installation authority first, else a public repository read
 * without credentials. Null when neither applies; a GitHub failure other than not-found fails.
 */
export const resolveReadableRepository = Effect.fn("Github.resolveReadableRepository")(function* (
  organizationId: string,
  repository: string,
) {
  const name = normalizePublicGithubRepository(repository);
  if (name === null) return null;
  const cached = yield* findGithubRepositoryByNameForOrganization({ organizationId, fullName: name });
  if (cached !== null) return cached satisfies ReadableRepository;
  const api = yield* GithubApi;
  const observed = yield* api.json({
    installationId: null, url: `https://api.github.com/repos/${name}`, operation: "resolve_repository", schema: PublicRepository,
  }).pipe(Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)));
  if (observed === null || observed.private) return null;
  return {
    fullName: observed.full_name, repositoryId: observed.id, installationId: null, defaultBranch: observed.default_branch,
  } satisfies ReadableRepository;
});

/** Whether `branch` exists in the repository, read with the repository's own authority. */
export const githubBranchExists = Effect.fn("Github.branchExists")(function* (repository: ReadableRepository, branch: string) {
  const head = yield* resolveGithubBranchHead(repository.installationId,
    { id: repository.repositoryId, fullName: repository.fullName }, `refs/heads/${branch}`).pipe(
    // A name GitHub can't hold as a branch ref simply isn't one.
    Effect.catchIf((error) => error.code === "invalid_input", () => Effect.succeed({ state: "absent" as const })),
  );
  return head.state === "present";
});
