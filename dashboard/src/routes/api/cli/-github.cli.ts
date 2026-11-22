import { Effect } from "effect";
import { disconnectGithub, githubBranches, githubConnection, GithubRepositories } from "#/modules/github/github-cli.server";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import type { CliRoutes } from "#/server/cli-call";
import { cliRefusalResponse, NotFound } from "#/server/public-error";

/** The GitHub connection the CLI reads, and the repositories it reaches. */
export const githubCliRoutes = {
  "GET github": ({ caller }) =>
    requireClaimed(caller.organization.id, "github_connect").pipe(
      Effect.andThen(githubConnection(caller)),
      Effect.catchTag("StoreRefused", (refused) => Effect.succeed(cliRefusalResponse(refused.refusal))),
    ),
  "GET github/:id": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const query = new URL(request.url).searchParams;
      const repository = query.get("repository");
      const path = query.get("path");
      const optional = (key: string) => query.get(key) ?? undefined;
      if (repository === null) return yield* new NotFound({ message: "Not found." });
      const github = yield* GithubRepositories;
      const read = Effect.gen(function* () {
        switch (id) {
          case "branches":
            return yield* githubBranches(caller, repository);
          case "tree":
            return yield* github.tree(caller, { repository, path: optional("path"), ref: optional("ref"), match: optional("match") });
          case "file":
            if (path === null) return yield* new NotFound({ message: "Not found." });
            return yield* github.file(caller, { repository, path, ref: optional("ref") });
          default:
            return yield* new NotFound({ message: "Not found." });
        }
      });
      return yield* read.pipe(Effect.catchTag("StoreRefused", (refused) => Effect.succeed(cliRefusalResponse(refused.refusal))));
    }),
  "DELETE github/:id": ({ caller, id }) => disconnectGithub(caller, caller.organization.slug, id ?? ""),
} satisfies CliRoutes;
