import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { Context, Data, Effect, Layer, Redacted, Result, Schema } from "effect";
import {
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
  GITHUB_COMPARE_STATUSES,
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
import { AppConfig } from "#/server/config.server";

const GITHUB_API_VERSION = "2022-11-28";
export { GITHUB_CHECK_SUITE_CONCLUSIONS as GITHUB_API_CHECK_SUITE_CONCLUSIONS };
export { GITHUB_CHECK_SUITE_STATUSES as GITHUB_API_CHECK_SUITE_STATUSES };
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
  | "resolve_branch_head"
  | "compare_commits"
  | "fetch_check_suite"
  | "list_repositories"
  | "list_branches"
  | "installation_token";

export type GithubObservationErrorCode =
  | "invalid_input"
  | "request_failed"
  | "not_found"
  | "invalid_response"
  | "identity_mismatch";

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
const installationTokenResponseSchema = Schema.Struct({
  token: Schema.String.check(Schema.isMinLength(1)),
  expires_at: githubTimestampSchema,
});

export type GithubJsonRequest<S extends Schema.ConstraintDecoder<unknown>> = {
  installationId: number;
  url: string;
  operation: GithubObservationOperation;
  schema: S;
};

export interface GithubApiService {
  readonly json: <S extends Schema.ConstraintDecoder<unknown>>(
    input: GithubJsonRequest<S>,
  ) => Effect.Effect<S["Type"], GithubObservationError>;
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

export const resolveInstallationRepository = Effect.fn(
  "Github.resolveInstallationRepository",
)(function* (installationId: number, repositoryId: number) {
  const operation = "resolve_repository";
  if (!isValidGithubId(repositoryId)) {
    return yield* githubObservationError({
      code: "invalid_input",
      operation,
      retriable: false,
    });
  }

  const api = yield* GithubApi;
  const observed = yield* api.json({
    installationId,
    url: `https://api.github.com/repositories/${repositoryId}`,
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

export const resolveInstallationBranchHead = Effect.fn(
  "Github.resolveInstallationBranchHead",
)(function* (
  installationId: number,
  repository: GithubResolvedRepository,
  ref: string,
) {
  const operation = "resolve_branch_head";
  const repositoryPath = resolvedRepositoryPath(repository);
  if (!repositoryPath || !isValidGithubBranchRef(ref)) {
    return yield* githubObservationError({
      code: "invalid_input",
      operation,
      retriable: false,
    });
  }

  const gitRef = ref.slice("refs/".length);
  const api = yield* GithubApi;
  const observed = yield* api
    .json({
      installationId,
      url: `https://api.github.com/repos/${repositoryPath}/git/ref/${encodeURIComponent(gitRef)}`,
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
  installationId: number,
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
      code: "invalid_input",
      operation,
      retriable: false,
    });
  }

  const api = yield* GithubApi;
  const testimony = yield* api.json({
    installationId,
    url: `https://api.github.com/repos/${repositoryPath}/compare/${baseSha}...${headSha}`,
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
  installationId: number,
  repository: GithubResolvedRepository,
  checkSuiteId: number,
) {
  const operation = "fetch_check_suite";
  const repositoryPath = resolvedRepositoryPath(repository);
  if (!repositoryPath || !isValidGithubId(checkSuiteId)) {
    return yield* githubObservationError({
      code: "invalid_input",
      operation,
      retriable: false,
    });
  }

  const api = yield* GithubApi;
  const testimony = yield* api.json({
    installationId,
    url: `https://api.github.com/repos/${repositoryPath}/check-suites/${checkSuiteId}`,
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

export function createGithubAppJwt(input: {
  readonly appId: string;
  readonly privateKey: string;
}): string {
  const privateKey = input.privateKey.replace(/\\n/g, "\n");

  const now = Math.floor(Date.now() / 1000);
  const header = Buffer.from(
    JSON.stringify({ alg: "RS256", typ: "JWT" }),
  ).toString("base64url");
  const payload = Buffer.from(
    JSON.stringify({
      iss: input.appId,
      iat: now - 60,
      exp: now + 600,
    }),
  ).toString("base64url");

  const signature = crypto
    .createSign("RSA-SHA256")
    .update(`${header}.${payload}`)
    .sign(
      { key: privateKey, padding: crypto.constants.RSA_PKCS1_PADDING },
      "base64url",
    );

  return `${header}.${payload}.${signature}`;
}

export const GithubApiLive = Layer.effect(
  GithubApi,
  Effect.gen(function* () {
    const config = yield* AppConfig;
    const tokenCache = new Map<number, { token: string; expiresAt: Date }>();

    const installationToken = Effect.fn("GithubApi.installationToken")(
      function* (installationId: number) {
        if (!isValidGithubId(installationId)) {
          return yield* githubObservationError({
            code: "invalid_input",
            operation: "installation_token",
            retriable: false,
          });
        }
        const cached = tokenCache.get(installationId);
        if (cached && cached.expiresAt > new Date(Date.now() + 60_000)) {
          return cached.token;
        }

        const appId = config.github.appId;
        const appPrivateKey = config.github.appPrivateKey;
        if (appId === undefined || appPrivateKey === undefined) {
          return yield* githubObservationError({
            code: "request_failed",
            operation: "installation_token",
            retriable: false,
          });
        }
        const jwt = yield* Effect.try({
          try: () =>
            createGithubAppJwt({
              appId,
              privateKey: Redacted.value(appPrivateKey),
            }),
          catch: () =>
            githubObservationError({
              code: "request_failed",
              operation: "installation_token",
              retriable: false,
            }),
        });
        const response = yield* Effect.tryPromise({
          try: () =>
            fetch(
              `https://api.github.com/app/installations/${installationId}/access_tokens`,
              {
                method: "POST",
                headers: {
                  Authorization: `Bearer ${jwt}`,
                  Accept: "application/vnd.github+json",
                  "X-GitHub-Api-Version": GITHUB_API_VERSION,
                },
              },
            ),
          catch: () =>
            githubObservationError({
              code: "request_failed",
              operation: "installation_token",
              retriable: true,
            }),
        });
        if (!response.ok) {
          return yield* githubObservationError({
            code: response.status === 404 ? "not_found" : "request_failed",
            operation: "installation_token",
            status: response.status,
            retryAfterSeconds: retryAfterSeconds(response),
            retriable:
              response.status === 403 || isRetriableGithubResponse(response),
          });
        }

        const parsedJson = yield* Effect.tryPromise({
          try: async () => {
            const body: unknown = await response.json();
            return body;
          },
          catch: () =>
            githubObservationError({
              code: "request_failed",
              operation: "installation_token",
              status: response.status,
              retriable: true,
            }),
        });
        const decoded = yield* Schema.decodeUnknownEffect(
          installationTokenResponseSchema,
        )(parsedJson).pipe(
          Effect.mapError(() =>
            githubObservationError({
              code: "invalid_response",
              operation: "installation_token",
              status: response.status,
              retriable: false,
            }),
          ),
        );
        tokenCache.set(installationId, {
          token: decoded.token,
          expiresAt: new Date(decoded.expires_at),
        });
        return decoded.token;
      },
    );

    const json = Effect.fn("GithubApi.json")(
      function* <S extends Schema.ConstraintDecoder<unknown>>(
        input: GithubJsonRequest<S>,
      ) {
        if (!isValidGithubId(input.installationId)) {
          return yield* githubObservationError({
            code: "invalid_input",
            operation: input.operation,
            retriable: false,
          });
        }

        const token = yield* installationToken(input.installationId);
        const response = yield* Effect.tryPromise({
          try: () =>
            fetch(input.url, {
              headers: {
                Authorization: `Bearer ${token}`,
                Accept: "application/vnd.github+json",
                "X-GitHub-Api-Version": GITHUB_API_VERSION,
              },
            }),
          catch: () =>
            githubObservationError({
              code: "request_failed",
              operation: input.operation,
              retriable: true,
            }),
        });
        if (!response.ok) {
          return yield* githubObservationError({
            code: response.status === 404 ? "not_found" : "request_failed",
            operation: input.operation,
            status: response.status,
            retryAfterSeconds: retryAfterSeconds(response),
            retriable: isRetriableGithubResponse(response),
          });
        }
        const parsedJson = yield* Effect.tryPromise({
          try: async () => {
            const body: unknown = await response.json();
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

    return { json };
  }),
);

