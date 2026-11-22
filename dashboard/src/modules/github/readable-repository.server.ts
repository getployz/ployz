import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { Minimatch } from "minimatch";
import { Validation } from "#/server/public-error";
import { accessFor, type GithubAccess } from "./github-access";
import {
  GithubApi, GithubObservationError, isGithubObservationNotFound, listGithubRepositoryFiles, resolveGithubBranchHead, resolveGithubRepository,
} from "./github-observation.api";
import { githubIdSchema, githubRepositoryFullNameSchema } from "./github-ingestion.contracts";
import { findGithubRepositoryByNameForOrganization, getCachedGithubRepositoryForOrganization } from "./github.repository";
import { normalizePublicGithubRepository } from "./public-repository";

/** A repository the Organization may read, and how: through a linked installation, or publicly (`installationId` null). */
export type ReadableRepository = {
  readonly organizationId: string;
  readonly fullName: string;
  readonly repositoryId: number;
  readonly installationId: number | null;
  readonly defaultBranch: string;
};

const publicRepositoryFields = {
  id: githubIdSchema,
  full_name: githubRepositoryFullNameSchema,
  default_branch: Schema.NonEmptyString,
};
const PublicRepository = Schema.Struct({ ...publicRepositoryFields, private: Schema.Boolean });
const NotPrivateRepository = Schema.Struct({ ...publicRepositoryFields, private: Schema.Literal(false) });

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
  if (cached !== null) return { organizationId, ...cached } satisfies ReadableRepository;
  const api = yield* GithubApi;
  const observed = yield* api.json({
    access: { type: "public" }, path: `/repos/${name}`, operation: "resolve_repository", schema: PublicRepository,
  }).pipe(Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)));
  if (observed === null || observed.private) return null;
  return {
    organizationId, fullName: observed.full_name, repositoryId: observed.id, installationId: null, defaultBranch: observed.default_branch,
  } satisfies ReadableRepository;
});

const RepositoryOwner = Schema.Struct({ owner: Schema.Struct({ id: githubIdSchema }) });

/** The GitHub ID of `repository`'s owner, which an automatic deploy rule names beside the repository's own ID. */
export const repositoryOwnerId = Effect.fn("Github.repositoryOwnerId")(function* (repository: ReadableRepository) {
  const api = yield* GithubApi;
  const observed = yield* api.json({
    access: accessFor(repository.organizationId, repository.installationId),
    path: `/repositories/${repository.repositoryId}`,
    operation: "resolve_repository",
    schema: RepositoryOwner,
  });
  return observed.owner.id;
});

/** Whether `branch` exists in the repository, read with the repository's own authority. */
export const githubBranchExists = Effect.fn("Github.branchExists")(function* (repository: ReadableRepository, branch: string) {
  const head = yield* resolveGithubBranchHead(accessFor(repository.organizationId, repository.installationId),
    { id: repository.repositoryId, fullName: repository.fullName }, `refs/heads/${branch}`).pipe(
    // A name GitHub can't hold as a branch ref simply isn't one.
    Effect.catchIf((error) => error instanceof GithubObservationError && error.code === "invalid_argument", () => Effect.succeed({ state: "absent" as const })),
  );
  return head.state === "present";
});

/**
 * The repository `repositoryId` as this Organization may read it, or null when it can't: through its linked installation's
 * cache, or, with no installation, as a public repository (a private one reads as absent to the public API).
 */
export const resolveReadableRepositoryById = Effect.fn("Github.resolveReadableRepositoryById")(function* (input: {
  readonly organizationId: string; readonly installationId: number | null; readonly repositoryId: number;
}) {
  const access: GithubAccess = accessFor(input.organizationId, input.installationId);
  const repository = input.installationId === null
    ? yield* resolveGithubRepository(access, input.repositoryId).pipe(Effect.catchTag("GithubObservationError",
      (error) => error.code === "not_found" ? Effect.succeed(null) : Effect.fail(error)))
    : yield* getCachedGithubRepositoryForOrganization({
      organizationId: input.organizationId, installationId: input.installationId, repositoryId: input.repositoryId,
    });
  return repository === null ? null : { access, fullName: repository.fullName };
});

export const resolvePublicGithubRepository = Effect.fn("Github.resolvePublicRepository")(function* (input: string) {
  const name = normalizePublicGithubRepository(input);
  if (!name) return yield* new Validation({ message: "Enter owner/repo or a GitHub repository URL. Select the branch separately." });
  const api = yield* GithubApi;
  const repo = yield* api.json({ access: { type: "public" }, path: `/repos/${name}`, operation: "resolve_repository", schema: NotPrivateRepository });
  return { fullName: repo.full_name, repositoryId: repo.id, access: { type: "public" as const }, defaultBranch: repo.default_branch };
});

/**
 * A user-supplied glob over repository paths: `*` and `?` within a segment, `**` across them, dotfiles included. Basic
 * globs only, so a pattern can't brace-expand without bound.
 */
export const basicGlob = (pattern: string) =>
  new Minimatch(pattern, { dot: true, nobrace: true, noext: true, nonegate: true, nocomment: true });

/**
 * The files of a repository at `ref` that `query` selects: the named file itself or those under `directory`, matching
 * the glob `match`, at most `cap`. `sorted` orders them by name before the cap; otherwise GitHub's tree order stands.
 */
export const repositoryPaths = Effect.fn("Github.repositoryPaths")(function* (
  access: GithubAccess, fullName: string, ref: string,
  query: { readonly cap: number; readonly directory?: string; readonly match?: string; readonly sorted?: boolean },
) {
  const listed = yield* listGithubRepositoryFiles(access, fullName, ref);
  const { directory = "" } = query;
  const under = directory === "" ? listed.paths : listed.paths.filter((file) => file === directory || file.startsWith(`${directory}/`));
  const matcher = query.match === undefined ? null : basicGlob(query.match);
  const matching = matcher === null ? under : under.filter((file) => matcher.match(file));
  const paths = query.sorted === true ? matching.sort() : matching;
  return { paths: paths.slice(0, query.cap), truncated: listed.truncated || paths.length > query.cap };
});
