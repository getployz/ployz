import { createServerFn } from "@tanstack/react-start";
import { setResponseHeader } from "@tanstack/react-start/server";
import { Schema } from "effect";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";
import {
  resolvePublicGithubRepository,
  getGithubWeb,
  getGithubRepoAccessState,
  listGithubBranches,
  listGithubRepositories,
  requestGithubRepoSync,
  searchGithubFiles,
} from "#/modules/github/github.server";
import { listGithubBuildRepositories } from "#/modules/github/github-build.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const authenticated = [publicErrorMiddleware, actorMiddleware] as const;
const OrganizationScope = Schema.Struct({
  organizationSlug: Schema.NonEmptyString.check(Schema.isMaxLength(256)),
});
const RepositoryIdentity = Schema.Struct({
  ...OrganizationScope.fields,
  repositoryId: githubIdSchema,
  installationId: Schema.NullOr(githubIdSchema),
});

export const searchGithubFilesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({
    ...RepositoryIdentity.fields,
    ref: Schema.NonEmptyString.check(Schema.isMaxLength(256)),
    pattern: Schema.NonEmptyString.check(Schema.isMaxLength(256)),
  })))
  .handler(({ context, data }) => runActor(context, searchGithubFiles(context.actor, data)));

export const getGithubWebServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, getGithubWeb()));

export const getGithubRepoAccessStateServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationScope))
  .handler(({ context, data }) =>
    runActor(context, getGithubRepoAccessState(context.actor, data.organizationSlug)),
  );

export const requestGithubRepoSyncServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationScope))
  .handler(({ context, data }) =>
    runActor(context, requestGithubRepoSync(context.actor, data.organizationSlug)),
  );

export const listGithubBranchesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(RepositoryIdentity))
  .handler(({ context, data }) =>
    runActor(context, listGithubBranches(context.actor, data)),
  );

export const resolvePublicGithubRepositoryServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ repository: Schema.String.check(Schema.isMaxLength(500)) })))
  .handler(({ context, data }) => runActor(context, resolvePublicGithubRepository(data.repository)));

export const listGithubRepositoriesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationScope))
  .handler(({ context, data }) => {
    setResponseHeader("cache-control", "private, no-store");
    return runActor(context, listGithubRepositories(context.actor, data.organizationSlug));
  });

export const listGithubBuildRepositoriesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationScope))
  .handler(({ context, data }) => runActor(context, listGithubBuildRepositories(context.actor, data)));
