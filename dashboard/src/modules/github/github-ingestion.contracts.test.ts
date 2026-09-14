import { describe, expect, it } from "vitest";
import { Option, Schema } from "effect";
import {
  githubChangedPathsSchema,
  githubCheckSuiteConclusionSchema,
  githubCheckSuiteStatusSchema,
  githubExactShaSchema,
  githubIdSchema,
  githubRepositoryPathSchema,
  githubSafeBranchRefSchema,
  isValidGithubBranchRef,
  isValidGithubExactSha,
  isValidGithubId,
} from "#/modules/github/github-ingestion.contracts";

describe("GitHub ingestion canonical contracts", () => {
  it("normalizes transport values into one storage-neutral representation", () => {
    expect(Schema.decodeUnknownSync(githubIdSchema)(42)).toBe(42);
    expect(
      Schema.decodeUnknownSync(githubSafeBranchRefSchema)(
        "refs/heads/release/v1",
      ),
    ).toBe(
      "refs/heads/release/v1",
    );
    expect(Schema.decodeUnknownSync(githubExactShaSchema)("A".repeat(40))).toBe(
      "a".repeat(40),
    );
    expect(
      Schema.decodeUnknownSync(githubChangedPathsSchema)([
          "src/z.ts",
          "README.md",
          "src/z.ts",
        ]),
    ).toEqual(["README.md", "src/z.ts"]);
    expect(
      Schema.decodeUnknownSync(githubCheckSuiteStatusSchema)("completed"),
    ).toBe("completed");
    expect(
      Schema.decodeUnknownSync(githubCheckSuiteConclusionSchema)("success"),
    ).toBe("success");
  });

  it("rejects invalid ids, refs, SHAs, paths, and suite testimony", () => {
    for (const value of [
      0,
      -1,
      1.5,
      Number.MAX_SAFE_INTEGER + 1,
      Number.POSITIVE_INFINITY,
      Number.NaN,
    ]) {
      expect(
        Option.isNone(Schema.decodeUnknownOption(githubIdSchema)(value)),
      ).toBe(true);
    }
    for (const value of [
      "refs/tags/v1",
      "refs/heads/release/../main",
      "refs/heads/release\\main",
      "refs/heads/.hidden",
    ]) {
      expect(
        Option.isNone(
          Schema.decodeUnknownOption(githubSafeBranchRefSchema)(value),
        ),
      ).toBe(true);
    }
    expect(
      Option.isNone(Schema.decodeUnknownOption(githubExactShaSchema)("abc")),
    ).toBe(true);
    for (const value of ["/etc/passwd", "src/../secret", "src\\main.ts"]) {
      expect(
        Option.isNone(
          Schema.decodeUnknownOption(githubRepositoryPathSchema)(value),
        ),
      ).toBe(true);
    }
    expect(
      Option.isNone(
        Schema.decodeUnknownOption(githubCheckSuiteStatusSchema)("mystery"),
      ),
    ).toBe(true);
    expect(
      Option.isNone(
        Schema.decodeUnknownOption(githubCheckSuiteConclusionSchema)("maybe"),
      ),
    ).toBe(true);
  });

  it("guards accept every encoded form the schema accepts", () => {
    const upper = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
    expect(isValidGithubExactSha(upper)).toBe(true);
    expect(isValidGithubExactSha(upper.toLowerCase())).toBe(true);
    expect(isValidGithubExactSha(upper.slice(1))).toBe(false);
    expect(isValidGithubId(42)).toBe(true);
    expect(isValidGithubId(0)).toBe(false);
    expect(isValidGithubBranchRef("refs/heads/release/v1")).toBe(true);
    expect(isValidGithubBranchRef("refs/heads/../x")).toBe(false);
  });
});
