import { describe, expect, it } from "vitest";
import { Result } from "effect";
import {
  canonicalizeGithubChangedPaths,
  decodeGithubCheckSuitePayload,
  decodeGithubPushPayload,
  matchGithubWatchPaths,
} from "#/modules/github/github-webhook-contracts";

describe("GitHub webhook contracts", () => {
  it("decodes a branch push into a normalized, privacy-minimal contract", () => {
    const decoded = decodeGithubPushPayload({
      installation: { id: 17 },
      repository: { id: 42, full_name: "ployz/example" },
      ref: "refs/heads/main",
      before: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
      after: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
      created: false,
      deleted: false,
      forced: true,
      sender: { id: 99, login: "private-user" },
      commits: [{ id: "secret", message: "private commit message" }],
    });

    expect(Result.isSuccess(decoded)).toBe(true);
    if (Result.isFailure(decoded)) return;
    expect(decoded.success).toEqual({
      kind: "push",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: "main",
      beforeSha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      afterSha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      created: false,
      deleted: false,
      forced: true,
    });
    expect(decoded.success).not.toHaveProperty("sender");
    expect(decoded.success).not.toHaveProperty("commits");
  });

  it("accepts GitHub's signed all-zero SHA sentinels for created and deleted pushes", () => {
    for (const push of [
      {
        before: "0".repeat(40),
        after: "a".repeat(40),
        created: true,
        deleted: false,
      },
      {
        before: "a".repeat(40),
        after: "0".repeat(40),
        created: false,
        deleted: true,
      },
    ]) {
      const result = decodeGithubPushPayload({
        installation: { id: 17 },
        repository: { id: 42 },
        ref: "refs/heads/main",
        forced: false,
        ...push,
      });
      expect(Result.isSuccess(result)).toBe(true);
    }
  });

  it("decodes a supported check-suite transition without retaining raw GitHub data", () => {
    const decoded = decodeGithubCheckSuitePayload({
      action: "completed",
      installation: { id: 17 },
      repository: { id: 42, full_name: "ployz/example" },
      check_suite: {
        id: 9001,
        head_sha: "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
        status: "completed",
        conclusion: "success",
        updated_at: "2026-07-16T07:00:00Z",
        pull_requests: [{ id: 123 }],
      },
      sender: { id: 99, login: "private-user" },
    });

    expect(Result.isSuccess(decoded)).toBe(true);
    if (Result.isFailure(decoded)) return;
    expect(decoded.success).toEqual({
      kind: "check_suite",
      action: "completed",
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 9001,
      headSha: "cccccccccccccccccccccccccccccccccccccccc",
      status: "completed",
      conclusion: "success",
      sourceUpdatedAt: "2026-07-16T07:00:00.000Z",
    });
    expect(decoded.success).not.toHaveProperty("sender");
    expect(decoded.success).not.toHaveProperty("pull_requests");
  });

  it("canonicalizes repository-relative changed paths deterministically", () => {
    const canonical = canonicalizeGithubChangedPaths([
      "src/z.ts",
      "README.md",
      "src/a.ts",
      "README.md",
    ]);

    expect(Result.isSuccess(canonical)).toBe(true);
    if (Result.isFailure(canonical)) return;
    expect(canonical.success).toEqual(["README.md", "src/a.ts", "src/z.ts"]);
  });

  it("rejects unsafe webhook identity, ref, SHA, action, status, and timestamp fields", () => {
    const validPush = {
      installation: { id: 17 },
      repository: { id: 42 },
      ref: "refs/heads/main",
      before: "a".repeat(40),
      after: "b".repeat(40),
      created: false,
      deleted: false,
      forced: false,
    };
    for (const mutation of [
      { installation: { id: 0 } },
      { ref: "refs/tags/v1" },
      { ref: "refs/heads/release/../main" },
      { ref: "refs/heads/release\\main" },
      { ref: "refs/heads/release\0main" },
      { ref: "refs/heads/release/@{main" },
      { ref: "refs/heads/release//main" },
      { ref: "refs/heads/.hidden" },
      { after: "abc" },
    ]) {
      const result = decodeGithubPushPayload({ ...validPush, ...mutation });
      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(result.failure.code).toBe("malformed_payload");
      }
    }

    const validSuite = {
      action: "completed",
      installation: { id: 17 },
      repository: { id: 42 },
      check_suite: {
        id: 9001,
        head_sha: "c".repeat(40),
        status: "completed",
        conclusion: "success",
        updated_at: "2026-07-16T07:00:00Z",
      },
    };
    const unsupported = decodeGithubCheckSuitePayload({
      ...validSuite,
      action: "reopened",
    });
    expect(Result.isFailure(unsupported)).toBe(true);
    if (Result.isFailure(unsupported)) {
      expect(unsupported.failure.code).toBe("unsupported_action");
    }
    for (const checkSuiteMutation of [
      { status: "mystery" },
      { updated_at: "yesterday" },
    ]) {
      const malformed = decodeGithubCheckSuitePayload({
        ...validSuite,
        check_suite: { ...validSuite.check_suite, ...checkSuiteMutation },
      });
      expect(Result.isFailure(malformed)).toBe(true);
      if (Result.isFailure(malformed)) {
        expect(malformed.failure.code).toBe("malformed_payload");
      }
    }
  });

  it("applies ordered gitignore-style watch paths with last-match-wins", () => {
    const matched = matchGithubWatchPaths({
      changedPaths: [
        ".github/workflows/ci.yml",
        "nested/README.md",
        "src/docs/guide.md",
        "src/docs/ship.md",
      ],
      watchPaths: ["src/**", "!src/docs/**", "src/docs/ship.md"],
    });

    expect(Result.isSuccess(matched)).toBe(true);
    if (Result.isFailure(matched)) return;
    expect(matched.success).toBe(true);
  });

  it("supports root anchors, unanchored basenames, directories, globstars, and dotfiles", () => {
    const cases = [
      { changedPaths: ["src/main.ts"], watchPaths: [], expected: true },
      { changedPaths: [], watchPaths: [], expected: false },
      {
        changedPaths: ["nested/README.md"],
        watchPaths: ["README.md"],
        expected: true,
      },
      {
        changedPaths: ["nested/README.md"],
        watchPaths: ["/README.md"],
        expected: false,
      },
      {
        changedPaths: ["nested/docs/guide.md"],
        watchPaths: ["docs/"],
        expected: true,
      },
      {
        changedPaths: ["packages/api/src/index.ts"],
        watchPaths: ["packages/**/src/**"],
        expected: true,
      },
      {
        changedPaths: [".github/workflows/ci.yml"],
        watchPaths: ["**/.github/**"],
        expected: true,
      },
      {
        changedPaths: ["docs/guide.md"],
        watchPaths: ["src/**"],
        expected: false,
      },
      {
        changedPaths: ["src/docs/guide.md"],
        watchPaths: ["src/**", "!src/docs/**"],
        expected: false,
      },
    ];

    for (const input of cases) {
      const result = matchGithubWatchPaths(input);
      expect(Result.isSuccess(result)).toBe(true);
      if (Result.isSuccess(result)) expect(result.success).toBe(input.expected);
    }
  });

  it("rejects ambiguous changed paths and malformed or traversing watch patterns", () => {
    for (const changedPath of [
      "",
      "/etc/passwd",
      "C:/Windows/system.ini",
      "src\\main.ts",
      "src/../secret",
      "src/./main.ts",
      "src//main.ts",
      "src/evil\0name",
    ]) {
      const result = canonicalizeGithubChangedPaths([changedPath]);
      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(result.failure.code).toBe("invalid_changed_path");
      }
    }

    for (const watchPath of [
      "",
      "!",
      "!!src/**",
      "../src/**",
      "src/../secret",
      "src\\**",
      "src/[abc",
      "//src/**",
      "C:/src/**",
    ]) {
      const result = matchGithubWatchPaths({
        changedPaths: ["src/main.ts"],
        watchPaths: [watchPath],
      });
      expect(Result.isFailure(result)).toBe(true);
      if (Result.isFailure(result)) {
        expect(result.failure.code).toBe("invalid_watch_pattern");
      }
    }
  });

});
