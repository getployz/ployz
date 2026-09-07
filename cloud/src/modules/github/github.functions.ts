import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";
import {
  getGithubInstallUrl,
  getGithubRepoAccessState,
  listGithubBranches,
  requestGithubRepoSync,
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
  installationId: githubIdSchema,
});

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
