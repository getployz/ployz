import "@tanstack/react-start/server-only";

import crypto from "node:crypto";
import { Effect, Redacted, Schema } from "effect";
import {
  GithubApi,
  GithubObservationError,
} from "#/modules/github/github-observation.api";
import type {
  GithubBranch,
  GithubInstallationReposPage,
} from "#/modules/github/github";
import { AppConfig } from "#/server/config.server";

const GITHUB_REPOSITORIES_PAGE_SIZE = 100;
const GithubId = Schema.Finite.check(
  Schema.isInt(),
  Schema.isGreaterThan(0),
  Schema.isLessThanOrEqualTo(Number.MAX_SAFE_INTEGER),
);
const NonEmptyString = Schema.String.check(Schema.isMinLength(1));
const GithubRepositoryResponse = Schema.Struct({
  id: GithubId,
  name: NonEmptyString,
  full_name: NonEmptyString,
  private: Schema.Boolean,
  default_branch: NonEmptyString,
  html_url: NonEmptyString,
  updated_at: Schema.DateFromString,
});
const GithubRepositoriesPageResponse = Schema.Struct({
  repositories: Schema.Array(GithubRepositoryResponse),
  total_count: Schema.Finite.check(
    Schema.isInt(),
    Schema.isGreaterThanOrEqualTo(0),
    Schema.isLessThanOrEqualTo(Number.MAX_SAFE_INTEGER),
  ),
});
const GithubBranchesResponse = Schema.Array(
  Schema.Struct({ name: NonEmptyString }),
);
const RepositoryFullName = Schema.String.check(
  Schema.isPattern(/^[^/\s]+\/[^/\s]+$/),
);

export const listInstallationReposPage = Effect.fn(
  "Github.listInstallationReposPage",
)(function* (installationId: number, page: number) {
  if (!Number.isSafeInteger(page) || page < 1) {
    return yield* new GithubObservationError({
      code: "invalid_input",
      operation: "list_repositories",
      retriable: false,
      message: "GitHub list repositories failed.",
    });
  }
  const api = yield* GithubApi;
  const response = yield* api.json({
    installationId,
    url: `https://api.github.com/installation/repositories?per_page=${GITHUB_REPOSITORIES_PAGE_SIZE}&page=${page}`,
    operation: "list_repositories",
    schema: GithubRepositoriesPageResponse,
  });
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

export const listInstallationRepoBranches = Effect.fn(
  "Github.listInstallationRepoBranches",
)(function* (installationId: number, repositoryFullName: string) {
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
      installationId,
      url: `https://api.github.com/repos/${repository}/branches?per_page=100&page=${page}`,
      operation: "list_branches",
      schema: GithubBranchesResponse,
    });
    branches.push(...pageBranches);
    if (pageBranches.length < 100) break;
    page += 1;
  }
  return branches.sort((left, right) => left.name.localeCompare(right.name));
});

export const verifyWebhookSignature = Effect.fn("Github.verifyWebhookSignature")(
function* (
  body: string,
  signature: string | null,
){
  const config = yield* AppConfig;
  const secret = config.github.appWebhookSecret;
  if (!signature || !secret) return false;
  const expected = crypto
    .createHmac("sha256", Redacted.value(secret))
    .update(body)
    .digest("hex");
  const expectedBuffer = Buffer.from(`sha256=${expected}`);
  const actualBuffer = Buffer.from(signature);
  if (expectedBuffer.length !== actualBuffer.length) return false;
  return crypto.timingSafeEqual(expectedBuffer, actualBuffer);
});
