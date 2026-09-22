import { expect, it } from "vitest";
import { normalizePublicGithubRepository } from "./public-repository";

it.each(["owner/repo", "github.com/owner/repo", "https://github.com/owner/repo", "http://github.com/owner/repo", " https://github.com/owner/repo.git/ "])("normalizes %s", input => {
  expect(normalizePublicGithubRepository(input)).toBe("owner/repo");
});
it.each(["https://evil.test/owner/repo", "http://127.0.0.1/owner/repo", "https://github.com@evil.test/owner/repo", "https://user:password@github.com/owner/repo", "owner/repo/tree/main", "https://github.com/owner/repo?x=1", "owner/..", "owner/.git", "owner/repo#main", "git@github.com:owner/repo", "owner/repo/extra"])("rejects %s", input => {
  expect(normalizePublicGithubRepository(input)).toBeNull();
});

it.each([96, 97, 100, 101])("validates the repository name length without the .git suffix (%i)", length => {
  const name = "r".repeat(length);
  const expected = length <= 100 ? `owner/${name}` : null;
  for (const prefix of ["", "github.com/", "http://github.com/", "https://github.com/"]) {
    for (const suffix of ["", ".git", ".git/"]) {
      expect(normalizePublicGithubRepository(`${prefix}owner/${name}${suffix}`)).toBe(expected);
    }
  }
});
