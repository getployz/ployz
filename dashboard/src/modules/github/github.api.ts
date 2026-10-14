import "@tanstack/react-start/server-only";

import crypto from "node:crypto";
import { Effect, Redacted, Schema } from "effect";
import type { GithubAccess } from "#/modules/github/github-access";
import {
  GITHUB_REPOSITORIES_PAGE_SIZE,
  GithubApi,
  GithubObservationError,
} from "#/modules/github/github-observation.api";
import type {
  GithubBranch,
  GithubInstallationReposPage,
} from "#/modules/github/github";
import { AppConfig } from "#/server/config.server";
import { githubExactShaSchema, githubRepositoryPathSchema } from "./github-ingestion.contracts";

const NonEmptyString = Schema.String.check(Schema.isMinLength(1));
const GithubBranchesResponse = Schema.Array(
  Schema.Struct({ name: NonEmptyString }),
);
const RepositoryFullName = Schema.String.check(
  Schema.isPattern(/^[^/\s]+\/[^/\s]+$/),
);

export const listInstallationReposPage = Effect.fn(
  "Github.listInstallationReposPage",
)(function* (installationId: number, page: number) {
  const api = yield* GithubApi;
  const response = yield* api.installationRepositories({ installationId, page });
  return {
    repositories: response.repositories.map((repository) => ({
      id: repository.id,
      name: repository.name,
      full_name: repository.full_name,
      private: repository.private,
      default_branch: repository.default_branch,
      html_url: repository.html_url,
      repo_updated_at: repository.updated_at.toISOString(),
    })),
    page,
    totalCount: response.total_count,
    hasNextPage:
      page * GITHUB_REPOSITORIES_PAGE_SIZE < response.total_count,
  } satisfies GithubInstallationReposPage;
});

export const listGithubRepositoryBranches = Effect.fn(
  "Github.listGithubRepositoryBranches",
)(function* (access: GithubAccess, repositoryFullName: string) {
  const repository = yield* Schema.decodeUnknownEffect(RepositoryFullName)(
    repositoryFullName,
  ).pipe(
    Effect.mapError(
      () =>
        new GithubObservationError({
          code: "invalid_input",
          operation: "list_branches",
          retriable: false,
          message: "GitHub list branches failed.",
        }),
    ),
  );
  const api = yield* GithubApi;
  const branches: GithubBranch[] = [];
  let page = 1;
  while (true) {
    const pageBranches = yield* api.json({
      access,
      path: `/repos/${repository}/branches?per_page=100&page=${page}`,
      operation: "list_branches",
      schema: GithubBranchesResponse,
    });
    branches.push(...pageBranches);
    if (pageBranches.length < 100) break;
    page += 1;
  }
  return branches.sort((left, right) => left.name.localeCompare(right.name));
});

export const listGithubRepositoryFiles = Effect.fn("Github.listGithubRepositoryFiles")(
  function* (access: GithubAccess, repositoryFullName: string, ref: string) {
    const api = yield* GithubApi;
    const repositoryPath = repositoryFullName.split("/").map(encodeURIComponent).join("/");
    const base: `/${string}` = `/repos/${repositoryPath}`;
    const commit = yield* api.json({
      access,
      path: `${base}/commits/${encodeURIComponent(ref)}`,
      operation: "resolve_file_ref",
      schema: Schema.Struct({
        commit: Schema.Struct({ tree: Schema.Struct({ sha: githubExactShaSchema }) }),
      }),
    });
    const tree = yield* api.json({
      access,
      path: `${base}/git/trees/${commit.commit.tree.sha}?recursive=1`,
      operation: "list_files",
      schema: Schema.Struct({
        truncated: Schema.Boolean,
        tree: Schema.Array(Schema.Struct({
          path: githubRepositoryPathSchema,
          type: Schema.Literals(["blob", "tree", "commit"]),
          mode: Schema.String,
        })),
      }),
    });
    // GitHub bounds recursive trees to 100,000 entries / 7 MB; report partial results.
    return {
      paths: tree.tree.filter((entry) => entry.type === "blob" && entry.mode !== "120000")
        .map((entry) => entry.path),
      truncated: tree.truncated,
    };
  },
);

/** What GitHub's contents API holds at `path`: a directory's entries, or one file, its base64 `content` absent past 1 MB. */
export const readGithubRepositoryContents = Effect.fn("Github.readGithubRepositoryContents")(
  function* (access: GithubAccess, repositoryFullName: string, path: string, ref: string) {
    const api = yield* GithubApi;
    const repositoryPath = repositoryFullName.split("/").map(encodeURIComponent).join("/");
    const filePath = path.split("/").map(encodeURIComponent).join("/");
    return yield* api.json({
      access,
      path: `/repos/${repositoryPath}/contents/${filePath}?ref=${encodeURIComponent(ref)}`,
      operation: "fetch_contents",
      schema: Schema.Union([
        Schema.Array(Schema.Struct({ path: Schema.String })),
        Schema.Struct({
          type: Schema.String,
          size: Schema.Number,
          encoding: Schema.optional(Schema.String),
          content: Schema.optional(Schema.String),
        }),
      ]),
    });
  },
);

export const verifyWebhookSignature = Effect.fn("Github.verifyWebhookSignature")(
function* (
  body: string,
  signature: string | null,
){
  const config = yield* AppConfig;
  const secret = config.github.appWebhookSecret;
  if (!signature) return false;
  const expected = crypto
    .createHmac("sha256", Redacted.value(secret))
    .update(body)
    .digest("hex");
  const expectedBuffer = Buffer.from(`sha256=${expected}`);
  const actualBuffer = Buffer.from(signature);
  if (expectedBuffer.length !== actualBuffer.length) return false;
  return crypto.timingSafeEqual(expectedBuffer, actualBuffer);
});
