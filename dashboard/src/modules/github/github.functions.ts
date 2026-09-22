import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";
import {
  resolvePublicGithubRepository,
  getGithubInstallUrl,
  getGithubRepoAccessState,
  listGithubBranches,
  requestGithubRepoSync,
  searchGithubFiles,
} from "#/modules/github/github.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const authenticated = [publicErrorMiddleware, actorMiddleware] as const;
const RepositoryIdentity = Schema.Struct({
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

export const getGithubInstallUrlServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, getGithubInstallUrl()));

export const getGithubRepoAccessStateServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) =>
    runActor(context, getGithubRepoAccessState(context.actor)),
  );

export const requestGithubRepoSyncServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .handler(({ context }) =>
    runActor(context, requestGithubRepoSync(context.actor)),
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
