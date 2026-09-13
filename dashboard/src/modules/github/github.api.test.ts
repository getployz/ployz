import { afterEach, describe, expect, it, vi } from "vitest";
import { Effect, Layer, Result } from "effect";
import {
  listInstallationRepoBranches as listInstallationRepoBranchesEffect,
  listInstallationReposPage as listInstallationReposPageEffect,
} from "#/modules/github/github.api";
import {
  compareInstallationRepositoryCommits as compareInstallationRepositoryCommitsEffect,
  fetchInstallationCheckSuite as fetchInstallationCheckSuiteEffect,
  resolveInstallationBranchHead as resolveInstallationBranchHeadEffect,
  resolveInstallationRepository as resolveInstallationRepositoryEffect,
  GithubApiLive,
} from "#/modules/github/github-observation.api";
import { AppConfig } from "#/server/config.server";

const GithubTestLive = GithubApiLive.pipe(Layer.provideMerge(AppConfig.layer));

async function resolveInstallationRepository(
  ...args: Parameters<typeof resolveInstallationRepositoryEffect>
) {
  return Effect.runPromise(
    Effect.result(resolveInstallationRepositoryEffect(...args)).pipe(
      Effect.provide(GithubTestLive),
    ),
  );
}

async function resolveInstallationBranchHead(
  ...args: Parameters<typeof resolveInstallationBranchHeadEffect>
) {
  return Effect.runPromise(
    Effect.result(resolveInstallationBranchHeadEffect(...args)).pipe(
      Effect.provide(GithubTestLive),
    ),
  );
}

async function compareInstallationRepositoryCommits(
  ...args: Parameters<typeof compareInstallationRepositoryCommitsEffect>
) {
  return Effect.runPromise(
    Effect.result(compareInstallationRepositoryCommitsEffect(...args)).pipe(
      Effect.provide(GithubTestLive),
    ),
  );
}

async function fetchInstallationCheckSuite(
  ...args: Parameters<typeof fetchInstallationCheckSuiteEffect>
) {
  return Effect.runPromise(
    Effect.result(fetchInstallationCheckSuiteEffect(...args)).pipe(
      Effect.provide(GithubTestLive),
    ),
  );
}

async function listInstallationReposPage(
  ...args: Parameters<typeof listInstallationReposPageEffect>
) {
  return Effect.runPromise(
    Effect.result(listInstallationReposPageEffect(...args)).pipe(
      Effect.provide(GithubTestLive),
    ),
  );
}

async function listInstallationRepoBranches(
  ...args: Parameters<typeof listInstallationRepoBranchesEffect>
) {
  return Effect.runPromise(
    Effect.result(listInstallationRepoBranchesEffect(...args)).pipe(
      Effect.provide(GithubTestLive),
    ),
  );
}

function jsonResponse<T>(
  body: T,
  status = 200,
  headers: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

function mockFetchSequence(...responses: Response[]) {
  const fetchMock = vi.fn<typeof fetch>();
  for (const response of responses) {
    fetchMock.mockResolvedValueOnce(response);
  }
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function installationTokenResponse(): Response {
  return jsonResponse({
    token: "installation-secret",
    expires_at: "2099-01-01T00:00:00Z",
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("resolveInstallationRepository", () => {
  it("resolves a repository through its stable installation-scoped id", async () => {
    const fetchMock = mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ id: 9_001, full_name: "ployz/example-renamed" }),
    );

    const result = await resolveInstallationRepository(4_001, 9_001);

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({ id: 9_001, fullName: "ployz/example-renamed" });
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "https://api.github.com/repositories/9001",
      expect.objectContaining({
        headers: expect.objectContaining({
          Authorization: "Bearer installation-secret",
        }),
      }),
    );
  });

  it("rejects a response whose repository id does not match the request", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ id: 9_002, full_name: "ployz/wrong" }),
    );

    const result = await resolveInstallationRepository(4_002, 9_001);

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      _tag: "GithubObservationError",
      code: "identity_mismatch",
      operation: "resolve_repository",
      retriable: false,
    });
  });

  it("rejects a non-canonical full name from the stable repository endpoint", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ id: 9_001, full_name: "../repository" }),
    );

    const result = await resolveInstallationRepository(4_003, 9_001);

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure.code).toBe("invalid_response");
  });

  it("retries a 403 only when rate-limit headers prove throttling", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ message: "forbidden" }, 403),
      installationTokenResponse(),
      jsonResponse(
        { message: "secondary rate limit" },
        403,
        { "x-ratelimit-remaining": "0", "retry-after": "12" },
      ),
    );

    const forbidden = await resolveInstallationRepository(4_004, 9_001);
    const rateLimited = await resolveInstallationRepository(4_005, 9_001);

    expect(Result.isFailure(forbidden)).toBe(true);
    if (Result.isFailure(forbidden)) expect(forbidden.failure.retriable).toBe(false);
    expect(Result.isFailure(rateLimited)).toBe(true);
    if (Result.isFailure(rateLimited)) {
      expect(rateLimited.failure).toMatchObject({
        code: "request_failed",
        status: 403,
        retriable: true,
        retryAfterSeconds: 12,
      });
    }
  });
});

describe("resolveInstallationBranchHead", () => {
  const repository = { id: 9_001, fullName: "ployz/example-renamed" };

  it("resolves a slash-containing full branch ref to its exact commit", async () => {
    const fetchMock = mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({
        ref: "refs/heads/feature/storage/alarms",
        object: {
          type: "commit",
          sha: "a".repeat(40),
        },
      }),
    );

    const result = await resolveInstallationBranchHead(
      4_101,
      repository,
      "refs/heads/feature/storage/alarms",
    );

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({
      state: "present",
      ref: "refs/heads/feature/storage/alarms",
      headSha: "a".repeat(40),
    });
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "https://api.github.com/repos/ployz/example-renamed/git/ref/heads%2Ffeature%2Fstorage%2Falarms",
      expect.any(Object),
    );
  });

  it("returns an absent observation only for a ref endpoint 404", async () => {
    mockFetchSequence(installationTokenResponse(), jsonResponse({}, 404));

    const result = await resolveInstallationBranchHead(
      4_102,
      repository,
      "refs/heads/deleted",
    );

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({
      state: "absent",
      ref: "refs/heads/deleted",
    });
  });

  it("keeps retriable GitHub failures distinct from an absent ref", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ message: "temporary" }, 500),
    );

    const result = await resolveInstallationBranchHead(
      4_103,
      repository,
      "refs/heads/main",
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      _tag: "GithubObservationError",
      code: "request_failed",
      operation: "resolve_branch_head",
      status: 500,
      retriable: true,
    });
  });

  it("rejects an unsafe branch ref before requesting a token", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);

    const result = await resolveInstallationBranchHead(
      4_104,
      repository,
      "refs/heads/release/../main",
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure.code).toBe("invalid_input");
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("compareInstallationRepositoryCommits", () => {
  const repository = { id: 9_101, fullName: "ployz/compare-target" };
  const baseSha = "1".repeat(40);
  const headSha = "2".repeat(40);

  it("returns canonical sorted paths including both sides of a rename", async () => {
    const fetchMock = mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({
        status: "ahead",
        base_commit: { sha: baseSha },
        files: [
          { filename: "src/new.ts", previous_filename: "src/old.ts" },
          { filename: "README.md" },
          { filename: "src/new.ts" },
        ],
      }),
    );

    const result = await compareInstallationRepositoryCommits(
      4_201,
      repository,
      baseSha,
      headSha,
    );

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({
      status: "ahead",
      baseSha,
      headSha,
      changedPaths: ["README.md", "src/new.ts", "src/old.ts"],
      pathsComplete: true,
    });
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      `https://api.github.com/repos/ployz/compare-target/compare/${baseSha}...${headSha}`,
      expect.any(Object),
    );
  });

  it.each([
    [299, true],
    [300, false],
  ] as const)(
    "marks a %i-file comparison completeness as %s",
    async (fileCount, pathsComplete) => {
      mockFetchSequence(
        installationTokenResponse(),
        jsonResponse({
          status: "ahead",
          base_commit: { sha: baseSha },
          files: Array.from({ length: fileCount }, (_, index) => ({
            filename: `src/file-${index}.ts`,
          })),
        }),
      );

      const result = await compareInstallationRepositoryCommits(
        4_300 + fileCount,
        repository,
        baseSha,
        headSha,
      );

      expect(Result.isSuccess(result)).toBe(true);
      if (Result.isFailure(result)) return;
      expect(result.success.pathsComplete).toBe(pathsComplete);
      expect(result.success.changedPaths).toHaveLength(fileCount);
    },
  );

  it.each(["behind", "diverged", "identical"] as const)(
    "returns %s as a typed observation",
    async (status) => {
      mockFetchSequence(
        installationTokenResponse(),
        jsonResponse({
          status,
          base_commit: { sha: baseSha },
          files: [],
        }),
      );

      const result = await compareInstallationRepositoryCommits(
        4_400 + status.length,
        repository,
        baseSha,
        headSha,
      );

      expect(Result.isSuccess(result)).toBe(true);
      if (Result.isFailure(result)) return;
      expect(result.success.status).toBe(status);
    },
  );

  it.each([
    [4_501, { status: "unknown", base_commit: { sha: baseSha }, files: [] }],
    [
      4_502,
      { status: "ahead", base_commit: { sha: "3".repeat(40) }, files: [] },
    ],
    [4_503, { status: "ahead", base_commit: { sha: baseSha } }],
    [
      4_504,
      {
        status: "ahead",
        base_commit: { sha: baseSha },
        files: [{ filename: 123 }],
      },
    ],
  ])("rejects malformed compare testimony %#", async (installationId, body) => {
    mockFetchSequence(installationTokenResponse(), jsonResponse(body));

    const result = await compareInstallationRepositoryCommits(
      installationId,
      repository,
      baseSha,
      headSha,
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      code: "invalid_response",
      operation: "compare_commits",
      retriable: false,
    });
  });

  it("rejects non-canonical repository-relative paths", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({
        status: "ahead",
        base_commit: { sha: baseSha },
        files: [{ filename: "src/../private-key" }],
      }),
    );

    const result = await compareInstallationRepositoryCommits(
      4_505,
      repository,
      baseSha,
      headSha,
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure.code).toBe("invalid_response");
  });
});

describe("fetchInstallationCheckSuite", () => {
  const repository = { id: 9_201, fullName: "ployz/check-target" };
  const headSha = "c".repeat(40);
  const validSuite = {
    id: 7_001,
    head_sha: headSha,
    status: "completed",
    conclusion: "success",
    updated_at: "2026-07-16T07:00:00Z",
  };

  it("fetches and validates authoritative suite testimony by stable id", async () => {
    const fetchMock = mockFetchSequence(
      installationTokenResponse(),
      jsonResponse(validSuite),
    );

    const result = await fetchInstallationCheckSuite(
      4_601,
      repository,
      7_001,
    );

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({
      checkSuiteId: 7_001,
      headSha,
      status: "completed",
      conclusion: "success",
      updatedAt: "2026-07-16T07:00:00.000Z",
    });
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "https://api.github.com/repos/ployz/check-target/check-suites/7001",
      expect.any(Object),
    );
  });

  it.each([
    [4_602, { ...validSuite, id: 7_002 }, "identity_mismatch"],
    [4_603, { ...validSuite, head_sha: "short" }, "invalid_response"],
    [4_604, { ...validSuite, status: "mystery" }, "invalid_response"],
    [4_605, { ...validSuite, conclusion: "mystery" }, "invalid_response"],
    [4_606, { ...validSuite, updated_at: "yesterday" }, "invalid_response"],
  ] as const)(
    "rejects invalid authoritative suite testimony %#",
    async (installationId, body, code) => {
      mockFetchSequence(installationTokenResponse(), jsonResponse(body));

      const result = await fetchInstallationCheckSuite(
        installationId,
        repository,
        7_001,
      );

      expect(Result.isFailure(result)).toBe(true);
      if (Result.isSuccess(result)) return;
      expect(result.failure).toMatchObject({
        code,
        operation: "fetch_check_suite",
        retriable: false,
      });
    },
  );

  it("returns a typed authority error for suite 404 without exposing response data", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ message: "installation-secret must stay private" }, 404),
    );

    const result = await fetchInstallationCheckSuite(
      4_607,
      repository,
      7_001,
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      code: "not_found",
      operation: "fetch_check_suite",
      status: 404,
      retriable: false,
    });
    expect(JSON.stringify(result.failure)).not.toContain("installation-secret");
  });

  it("accepts nonterminal authoritative testimony with a null conclusion", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({
        ...validSuite,
        status: "in_progress",
        conclusion: null,
      }),
    );

    const result = await fetchInstallationCheckSuite(
      4_608,
      repository,
      7_001,
    );

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toMatchObject({
      status: "in_progress",
      conclusion: null,
    });
  });

  it("rejects a malformed token response without sending an API request", async () => {
    const fetchMock = mockFetchSequence(
      jsonResponse({ expires_at: "2099-01-01T00:00:00Z" }),
    );

    const result = await fetchInstallationCheckSuite(
      4_609,
      repository,
      7_001,
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      code: "invalid_response",
      operation: "installation_token",
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("retries a truncated installation-token body without sending an API request", async () => {
    const fetchMock = mockFetchSequence(
      new Response("not-json", {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const result = await fetchInstallationCheckSuite(
      4_610,
      repository,
      7_001,
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      code: "request_failed",
      operation: "installation_token",
      retriable: true,
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("retries a headerless secondary-limit token 403 without sending an API request", async () => {
    const fetchMock = mockFetchSequence(
      jsonResponse({ message: "You have exceeded a secondary rate limit" }, 403),
    );

    const result = await fetchInstallationCheckSuite(
      4_611,
      repository,
      7_001,
    );

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      code: "request_failed",
      operation: "installation_token",
      status: 403,
      retriable: true,
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

describe("GitHub repository synchronization provider", () => {
  it("decodes one installation repository page and removes provider-only fields", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({
        total_count: 101,
        repositories: [
          {
            id: 77,
            name: "api",
            full_name: "acme/api",
            private: true,
            default_branch: "main",
            html_url: "https://github.com/acme/api",
            updated_at: "2026-03-27T00:00:00.000Z",
            provider_only: "removed",
          },
        ],
      }),
    );

    const result = await listInstallationReposPage(8_101, 1);

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({
      repositories: [
        {
          id: 77,
          name: "api",
          full_name: "acme/api",
          private: true,
          default_branch: "main",
          html_url: "https://github.com/acme/api",
          repo_updated_at: "2026-03-27T00:00:00.000Z",
        },
      ],
      page: 1,
      totalCount: 101,
      hasNextPage: true,
    });
  });

  it("decodes, paginates, and sorts branch testimony", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse(
        Array.from({ length: 100 }, (_, index) => ({
          name: `release-${String(100 - index).padStart(3, "0")}`,
        })),
      ),
      jsonResponse([{ name: "main", provider_only: "removed" }]),
    );

    const result = await listInstallationRepoBranches(8_102, "acme/api");

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toHaveLength(101);
    expect(result.success[0]).toEqual({ name: "main" });
    expect(result.success.at(-1)).toEqual({ name: "release-100" });
  });

  it("rejects malformed finite numeric testimony", async () => {
    mockFetchSequence(
      installationTokenResponse(),
      jsonResponse({ total_count: Number.POSITIVE_INFINITY, repositories: [] }),
    );

    const result = await listInstallationReposPage(8_103, 1);

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) return;
    expect(result.failure).toMatchObject({
      code: "invalid_response",
      operation: "list_repositories",
      retriable: false,
    });
  });
});
