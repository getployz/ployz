import { describe, expect, it } from "vitest";
import { getGitRepoSelectorState } from "#/components/service-source-selector-state";

describe("service source selector helpers", () => {
  it("returns the no-installations selector state before empty repo state", () => {
    expect(
      getGitRepoSelectorState({
        configured: true,
        hasInstallations: false,
        repoCount: 0,
        filteredRepoCount: 0,
      }),
    ).toBe("no-installations");
  });
});
