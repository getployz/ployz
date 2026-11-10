import "@tanstack/react-start/server-only";
import { Context, Data, Effect, Layer, Result, Schema } from "effect";
import type { GithubAccess, GithubDisconnected } from "#/modules/github/github-access";
import {
  GITHUB_COMPARE_STATUSES,
  githubBranchNameSchema,
  githubChangedPathsSchema,
  githubCheckSuiteConclusionSchema,
  githubCheckSuiteStatusSchema,
  githubCompareStatusSchema,
  githubExactShaSchema,
  githubIdSchema,
  githubRepositoryFullNameSchema,
  githubResolvedRepositorySchema,
  githubSafeBranchRefSchema,
  githubTimestampSchema,
  isValidGithubBranchRef,
  isValidGithubExactSha,
  isValidGithubId,
  type GithubBranchHeadObservation,
  type GithubCheckSuiteConclusion,
  type GithubCheckSuiteObservation,
  type GithubCheckSuiteStatus,
  type GithubCompareObservation,
  type GithubCompareStatus,
  type GithubResolvedRepository,
} from "#/modules/github/github-ingestion.contracts";
import { GITHUB_API_VERSION, GithubAuthority } from "#/modules/github/github-authority.server";
import { GithubTokens } from "#/modules/github/github-token.server";
import { AppConfig } from "#/server/config.server";

type GithubRequestHeaders = { "X-GitHub-Api-Version": string; Accept?: string; Authorization?: string };
export { GITHUB_COMPARE_STATUSES };
export type {
  GithubBranchHeadObservation,
  GithubCheckSuiteObservation,
  GithubCompareObservation,
  GithubCompareStatus,
  GithubResolvedRepository,
};
export type GithubApiCheckSuiteStatus = GithubCheckSuiteStatus;
export type GithubApiCheckSuiteConclusion = GithubCheckSuiteConclusion;

export type GithubObservationOperation =
  | "resolve_repository"
  | "list_push_runs"
  | "resolve_branch_head"
  | "compare_commits"
  | "fetch_check_suite"
  | "list_repositories"
  | "list_branches"
  | "resolve_file_ref"
  | "list_files"
  | "fetch_contents"
  | "installation_token"
  | "download_source"
  | "fetch_workflow"
  | "dispatch_workflow"
  | "cancel_run"
  | "rerun_run"
  | "fetch_run"
  | "fetch_installation"
  | "fetch_pull_request"
  | "list_check_runs"
  | "create_check_run"
  | "update_check_run"
  | "exchange_user_code"
  | "list_user_installations"
  | "read_user_membership";

export type GithubObservationErrorCode =
  | "invalid_argument"
  | "request_failed"
  | "not_found"
  | "invalid_response"
  | "identity_mismatch"
  | "refused";

export class GithubObservationError extends Data.TaggedError(
  "GithubObservationError",
)<{
  code: GithubObservationErrorCode;
  operation: GithubObservationOperation;
  status?: number;
  retryAfterSeconds?: number;
  retriable: boolean;
  message: string;
}> {}

function githubObservationError(args: {
  code: GithubObservationErrorCode;
  operation: GithubObservationOperation;
  status?: number;
  retryAfterSeconds?: number;
  retriable: boolean;
}): GithubObservationError {
  return new GithubObservationError({
    ...args,
    message: `GitHub ${args.operation} failed (${args.code}).`,
  });
}

export function isGithubObservationNotFound(
  cause: unknown,
): cause is GithubObservationError {
  return cause instanceof GithubObservationError && cause.code === "not_found";
}

/** A missing permission is a 403 GitHub won't lift by waiting; rate limits are retriable 403s. */
export function githubLacksPermission(cause: unknown): cause is GithubObservationError {
  return cause instanceof GithubObservationError && cause.status === 403 && !cause.retriable;
}

function resolvedRepositoryPath(
  repository: GithubResolvedRepository,
): string | null {
  if (
    Result.isFailure(
      Schema.decodeUnknownResult(githubResolvedRepositorySchema)(repository, {
        onExcessProperty: "error",
      }),
    )
  ) {
    return null;
  }
  const [owner, name] = repository.fullName.split("/");
  if (!owner || !name) return null;
  return `${encodeURIComponent(owner)}/${encodeURIComponent(name)}`;
}

const repositoryResponseSchema = Schema.Struct({
  id: githubIdSchema,
  full_name: githubRepositoryFullNameSchema,
});
const branchRefResponseSchema = Schema.Struct({
  ref: githubSafeBranchRefSchema,
  object: Schema.Struct({
    type: Schema.Literal("commit"),
    sha: githubExactShaSchema,
  }),
});
const compareResponseSchema = Schema.Struct({
  status: githubCompareStatusSchema,
  base_commit: Schema.optionalKey(
    Schema.Struct({ sha: githubExactShaSchema }),
  ),
  files: Schema.Array(
    Schema.Struct({
      filename: Schema.String,
      previous_filename: Schema.optionalKey(Schema.String),
    }),
  ),
});
const checkSuiteResponseSchema = Schema.Struct({
  id: githubIdSchema,
  head_sha: githubExactShaSchema,
  status: githubCheckSuiteStatusSchema,
  conclusion: Schema.NullOr(githubCheckSuiteConclusionSchema),
  updated_at: githubTimestampSchema,
});
const pullRequestResponseSchema = Schema.Struct({
  number: githubIdSchema,
  state: Schema.Literals(["open", "closed"]),
  title: Schema.String,
  user: Schema.NullOr(Schema.Struct({ id: githubIdSchema, login: Schema.String.check(Schema.isMinLength(1)), type: Schema.String })),
  head: Schema.Struct({
    ref: githubBranchNameSchema, sha: githubExactShaSchema, repo: Schema.NullOr(Schema.Struct({ id: githubIdSchema })),
  }),
  base: Schema.Struct({ ref: githubBranchNameSchema }),
  merged: Schema.Boolean,
  merge_commit_sha: Schema.NullOr(githubExactShaSchema),
  commits: Schema.Number,
  updated_at: Schema.optional(Schema.String),
});
const nonEmptyString = Schema.String.check(Schema.isMinLength(1));
const installationRepositoriesSchema = Schema.Struct({
  repositories: Schema.Array(Schema.Struct({
    id: githubIdSchema,
    name: nonEmptyString,
    full_name: nonEmptyString,
    private: Schema.Boolean,
    default_branch: nonEmptyString,
    html_url: nonEmptyString,
    updated_at: Schema.DateFromString,
  })),
  total_count: Schema.Finite.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0), Schema.isLessThanOrEqualTo(Number.MAX_SAFE_INTEGER)),
});
export type GithubInstallationRepositoriesResponse = typeof installationRepositoriesSchema.Type;
export const GITHUB_REPOSITORIES_PAGE_SIZE = 100;

/** A read. Every write, and every Actions call, goes through `GithubAuthority`. */
export type GithubJsonRequest<S extends Schema.ConstraintDecoder<unknown>> = {
  access: GithubAccess;
  path: `/${string}`;
  operation: GithubObservationOperation;
  schema: S;
};

export type GithubReadError = GithubObservationError | GithubDisconnected;

export interface GithubApiService {
  readonly archive: (input: { access: GithubAccess; repository: GithubResolvedRepository; sha: string }) => Effect.Effect<Response, GithubReadError>;
  readonly json: <S extends Schema.ConstraintDecoder<unknown>>(
    input: GithubJsonRequest<S>,
  ) => Effect.Effect<S["Type"], GithubReadError>;
  /** The installation `access` reads through, once Cloud checked it may: linked to the Organization and not suspended. */
  readonly installationOf: (access: GithubAccess) => Effect.Effect<number, GithubReadError>;
  /** One page of what an installation grants, read as Cloud for the repository cache. Never repository contents. */
  readonly installationRepositories: (
    input: { installationId: number; page: number },
  ) => Effect.Effect<GithubInstallationRepositoriesResponse, GithubObservationError>;
}

export class GithubApi extends Context.Service<GithubApi, GithubApiService>()(
  "ployz/GithubApi",
) {}

function retryAfterSeconds(response: Response): number | undefined {
  const value = response.headers.get("retry-after");
  if (!value || !/^\d+$/.test(value)) return undefined;
  const seconds = Number(value);
  return Number.isSafeInteger(seconds) ? seconds : undefined;
}

function isRetriableGithubResponse(response: Response): boolean {
  if (response.status === 429 || response.status >= 500) return true;
  if (response.status !== 403) return false;
  return (
    response.headers.get("x-ratelimit-remaining") === "0" ||
    retryAfterSeconds(response) !== undefined
  );
}

/** What a GitHub response that failed means: not found, or failed, retriable when waiting can lift it. */
export function githubResponseError(response: Response, operation: GithubObservationOperation): GithubObservationError {
  return githubObservationError({
    code: response.status === 404 ? "not_found" : "request_failed",
    operation,
    status: response.status,
    retryAfterSeconds: retryAfterSeconds(response),
    retriable: isRetriableGithubResponse(response),
  });
}

export const resolveGithubRepository = Effect.fn(
  "Github.resolveGithubRepository",
)(function* (access: GithubAccess, repositoryId: number) {
  const operation = "resolve_repository";
  if (!isValidGithubId(repositoryId)) {
    return yield* githubObservationError({
      code: "invalid_argument",
      operation,
      retriable: false,
    });
  }

  const api = yield* GithubApi;
  const observed = yield* api.json({
    access,
    path: `/repositories/${repositoryId}`,
    operation,
    schema: repositoryResponseSchema,
  });
  if (observed.id !== repositoryId) {
    return yield* githubObservationError({
      code: "identity_mismatch",
      operation,
      retriable: false,
    });
  }
  return {
    id: repositoryId,
    fullName: observed.full_name,
  };
});

export const resolveGithubBranchHead = Effect.fn(
  "Github.resolveGithubBranchHead",
)(function* (
  access: GithubAccess,
  repository: GithubResolvedRepository,
  ref: string,
) {
  const operation = "resolve_branch_head";
  const repositoryPath = resolvedRepositoryPath(repository);
  if (!repositoryPath || !isValidGithubBranchRef(ref)) {
    return yield* githubObservationError({
      code: "invalid_argument",
      operation,
      retriable: false,
    });
  }

  const gitRef = ref.slice("refs/".length);
  const api = yield* GithubApi;
  const observed = yield* api
    .json({
      access,
      path: `/repos/${repositoryPath}/git/ref/${encodeURIComponent(gitRef)}`,
      operation,
      schema: branchRefResponseSchema,
    })
    .pipe(
      Effect.map((value) => ({ state: "present" as const, value })),
      Effect.catchIf(isGithubObservationNotFound, () =>
        Effect.succeed({ state: "absent" as const }),
      ),
    );
  if (observed.state === "absent") {
    return { state: "absent" as const, ref };
  }
  if (observed.value.ref !== ref) {
    return yield* githubObservationError({
      code: "invalid_response",
      operation,
      retriable: false,
    });
  }

  return {
    state: "present" as const,
    ref,
    headSha: observed.value.object.sha,
  };
});

export const compareInstallationRepositoryCommits = Effect.fn(
  "Github.compareInstallationRepositoryCommits",
)(function* (
  access: GithubAccess,
  repository: GithubResolvedRepository,
  baseSha: string,
  headSha: string,
) {
  const operation = "compare_commits";
  const repositoryPath = resolvedRepositoryPath(repository);
  if (
    !repositoryPath ||
    !isValidGithubExactSha(baseSha) ||
    !isValidGithubExactSha(headSha)
  ) {
    return yield* githubObservationError({
      code: "invalid_argument",
      operation,
      retriable: false,
    });
  }

  const api = yield* GithubApi;
  const testimony = yield* api.json({
    access,
    path: `/repos/${repositoryPath}/compare/${baseSha}...${headSha}`,
    operation,
    schema: compareResponseSchema,
  });
  if (testimony.base_commit && testimony.base_commit.sha !== baseSha) {
    return yield* githubObservationError({
      code: "invalid_response",
      operation,
      retriable: false,
    });
  }

  const paths = testimony.files.flatMap((file) =>
    file.previous_filename
      ? [file.filename, file.previous_filename]
      : [file.filename],
  );
  const canonicalPaths = yield* Schema.decodeUnknownEffect(
    githubChangedPathsSchema,
  )(paths).pipe(
    Effect.mapError(() =>
      githubObservationError({
        code: "invalid_response",
        operation,
        retriable: false,
      }),
    ),
  );

  return {
    status: testimony.status,
    baseSha,
    headSha,
    changedPaths: canonicalPaths,
    pathsComplete: testimony.files.length < 300,
  };
});

export const fetchInstallationCheckSuite = Effect.fn(
  "Github.fetchInstallationCheckSuite",
)(function* (
  access: GithubAccess,
  repository: GithubResolvedRepository,
  checkSuiteId: number,
) {
  const operation = "fetch_check_suite";
  const repositoryPath = resolvedRepositoryPath(repository);
  if (!repositoryPath || !isValidGithubId(checkSuiteId)) {
    return yield* githubObservationError({
      code: "invalid_argument",
      operation,
      retriable: false,
    });
  }

  const api = yield* GithubApi;
  const testimony = yield* api.json({
    access,
    path: `/repos/${repositoryPath}/check-suites/${checkSuiteId}`,
    operation,
    schema: checkSuiteResponseSchema,
  });
  if (testimony.id !== checkSuiteId) {
    return yield* githubObservationError({
      code: "identity_mismatch",
      operation,
      retriable: false,
    });
  }
  return {
    checkSuiteId,
    headSha: testimony.head_sha,
    status: testimony.status,
    conclusion: testimony.conclusion,
    updatedAt: testimony.updated_at,
  };
});

/** A pull request as GitHub has it now, so a late or repeated delivery never acts on an older state. */
export const fetchInstallationPullRequest = Effect.fn(
  "Github.fetchInstallationPullRequest",
)(function* (access: GithubAccess, repositoryId: number, number: number) {
  const operation = "fetch_pull_request";
  if (!isValidGithubId(repositoryId) || !isValidGithubId(number)) {
    return yield* githubObservationError({ code: "invalid_argument", operation, retriable: false });
  }
  const api = yield* GithubApi;
  const observed = yield* api.json({
    access,
    path: `/repositories/${repositoryId}/pulls/${number}`,
    operation,
    schema: pullRequestResponseSchema,
  });
  if (observed.number !== number) {
    return yield* githubObservationError({ code: "identity_mismatch", operation, retriable: false });
  }
  return {
    open: observed.state === "open",
    title: observed.title,
    // A deleted author shows as GitHub's "ghost" user.
    author: { id: observed.user?.id ?? null, login: observed.user?.login ?? "ghost", isBot: observed.user?.type === "Bot" },
    headRepositoryId: observed.head.repo?.id ?? null,
    headBranch: observed.head.ref,
    headSha: observed.head.sha,
    targetBranch: observed.base.ref,
    commits: observed.commits,
    // Only a merged pull request's is its merge commit.
    mergeCommitSha: observed.merged ? observed.merge_commit_sha : null,
    // GitHub always sends it; the Config Store orders pull request facts by it.
    updatedAt: observed.updated_at ?? null,
  };
});

/** The permissions an installation of the GitHub App has granted, and where its owner manages them. */
export const fetchAppInstallationPermissions = Effect.fn(
  "Github.fetchAppInstallationPermissions",
)(function* (installationId: number) {
  return yield* (yield* GithubAuthority).installation(installationId);
});

/**
 * The installation's completed check run `name` on a commit for `externalId`: updated in place when the commit already
 * has that one, else created. Two checks of one name on one commit (two pull requests from one head) stay apart by
 * `externalId`. The caller serializes posts per check, so two never race to create.
 */
export const postInstallationCheckRun = Effect.fn(
  "Github.postInstallationCheckRun",
)(function* (access: GithubAccess, repositoryId: number, input: {
  headSha: string; name: string; externalId: string; conclusion: "success" | "neutral"; detailsUrl: string; title: string; summary: string;
}) {
  if (!isValidGithubId(repositoryId) || !isValidGithubExactSha(input.headSha)) {
    return yield* githubObservationError({ code: "invalid_argument", operation: "create_check_run", retriable: false });
  }
  const installationId = yield* (yield* GithubApi).installationOf(access);
  return yield* (yield* GithubAuthority).postCheck({ installationId, repositoryId, ...input });
});

export const GithubApiLive = Layer.effect(
  GithubApi,
  Effect.gen(function* () {
    const config = yield* AppConfig;
    const tokens = yield* GithubTokens;

    const bearer = (access: GithubAccess) => {
      if (access.type === "public") return Effect.succeed(null);
      if (access.type === "installation") return tokens.systemInstallationToken(access.installationId);
      return tokens.installationToken(access.organizationId, access.installationId);
    };

    const request = Effect.fn("GithubApi.request")(
      function* <S extends Schema.ConstraintDecoder<unknown>>(input: {
        token: string | null;
        path: `/${string}`;
        operation: GithubObservationOperation;
        schema: S;
      }) {
        const headers: GithubRequestHeaders = { Accept: "application/vnd.github+json", "X-GitHub-Api-Version": GITHUB_API_VERSION };
        if (input.token !== null) headers.Authorization = `Bearer ${input.token}`;
        const response = yield* Effect.tryPromise({
          try: (signal) => fetch(`${config.github.origins.api}${input.path}`, { signal, headers }),
          catch: () =>
            githubObservationError({
              code: "request_failed",
              operation: input.operation,
              retriable: true,
            }),
        });
        if (!response.ok) return yield* githubResponseError(response, input.operation);
        const parsedJson = yield* Effect.tryPromise({
          try: async () => {
            const text = await response.text();
            const body: unknown = text === "" ? {} : JSON.parse(text);
            return body;
          },
          catch: () =>
            githubObservationError({
              code: "invalid_response",
              operation: input.operation,
              status: response.status,
              retriable: false,
            }),
        });
        return yield* Schema.decodeUnknownEffect(input.schema)(parsedJson).pipe(
          Effect.mapError(() =>
            githubObservationError({
              code: "invalid_response",
              operation: input.operation,
              status: response.status,
              retriable: false,
            }),
          ),
        );
      },
    );

    const json: GithubApiService["json"] = Effect.fn("GithubApi.json")(
      function* <S extends Schema.ConstraintDecoder<unknown>>({ access, ...input }: GithubJsonRequest<S>) {
        if (access.type !== "public" && !isValidGithubId(access.installationId)) {
          return yield* githubObservationError({ code: "invalid_argument", operation: input.operation, retriable: false });
        }
        const token = yield* bearer(access);
        return yield* request({ ...input, token });
      },
    );

    const installationOf: GithubApiService["installationOf"] = (access) => access.type === "public"
      ? Effect.fail(githubObservationError({ code: "invalid_argument", operation: "installation_token", retriable: false }))
      : tokens.usable(access);

    const installationRepositories: GithubApiService["installationRepositories"] = Effect.fn("GithubApi.installationRepositories")(
      function* (input) {
        if (!isValidGithubId(input.installationId) || !Number.isSafeInteger(input.page) || input.page < 1) {
          return yield* githubObservationError({ code: "invalid_argument", operation: "list_repositories", retriable: false });
        }
        const token = yield* tokens.systemInstallationToken(input.installationId);
        return yield* request({
          token,
          path: `/installation/repositories?per_page=${GITHUB_REPOSITORIES_PAGE_SIZE}&page=${input.page}`,
          operation: "list_repositories",
          schema: installationRepositoriesSchema,
        });
      },
    );

    const archive: GithubApiService["archive"] = Effect.fn("GithubApi.archive")(function* (input) {
      const repository = resolvedRepositoryPath(input.repository);
      if (!repository || !isValidGithubExactSha(input.sha)) {
        return yield* githubObservationError({ code: "invalid_argument", operation: "download_source", retriable: false });
      }
      const token = yield* bearer(input.access);
      const headers: GithubRequestHeaders = { "X-GitHub-Api-Version": GITHUB_API_VERSION };
      if (token !== null) headers.Authorization = `Bearer ${token}`;
      return yield* Effect.tryPromise({
        try: async (signal) => {
          // Never forward installation credentials across the archive redirect.
          const redirect = await fetch(`${config.github.origins.api}/repos/${repository}/tarball/${input.sha}`, {
            signal, redirect: "manual", headers,
          });
          const location = redirect.headers.get("location");
          await redirect.body?.cancel();
          if (redirect.status !== 302 || !location) throw new Error("Archive redirect missing");
          const url = new URL(location);
          if (url.origin !== config.github.origins.codeload || url.username || url.password) throw new Error("Invalid archive destination");
          const response = await fetch(url, { signal, redirect: "error" });
          if (!response.ok || !response.body) { await response.body?.cancel(); throw new Error("Archive download failed"); }
          return response;
        },
        catch: () => githubObservationError({ code: "request_failed", operation: "download_source", retriable: false }),
      });
    });
    return { json, installationOf, installationRepositories, archive };
  }),
);
