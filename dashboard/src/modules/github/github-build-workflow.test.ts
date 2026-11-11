import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { githubBuildWorkflowDigests } from "./github-authority.server";
import {
  GITHUB_BUILD_WORKFLOW,
  GITHUB_BUILD_WORKFLOWS,
  GITHUB_BUILDER_COMMIT,
  githubBuildId,
  githubBuildWorkflowEditUrl,
  githubBuildWorkflowUrl,
} from "./github-build-workflow";

/** The minter's list, read from disk: the dashboard never imports from `minter/`. */
const minterDigests = () => {
  const listed: unknown = JSON.parse(readFileSync(new URL("../../../../minter/src/canonical-workflows.json", import.meta.url), "utf8"));
  return new Set((listed as { digests: string[] }).digests);
};

describe("GitHub build workflow", () => {
  it("calls the builder at a full commit with every input Cloud dispatches", () => {
    const uses = GITHUB_BUILD_WORKFLOWS.flatMap((workflow) => [...workflow.matchAll(/^\s*(?:-\s+)?uses:\s*(\S+)\s*$/gm)].map((found) => found[1]));
    expect(uses).toEqual([`getployz/build@${GITHUB_BUILDER_COMMIT}`]);
    expect(GITHUB_BUILDER_COMMIT).toMatch(/^[0-9a-f]{40}$/);
    for (const input of ["build", "cloud"]) {
      expect(GITHUB_BUILD_WORKFLOW).toContain(`${input}: \${{ inputs.${input} }}`);
    }
  });

  it("gives the run's GITHUB_TOKEN only contents: read and id-token: write", () => {
    for (const workflow of GITHUB_BUILD_WORKFLOWS) {
      const block = /^permissions:\n((?: {2}\S.*\n)+)/m.exec(workflow)?.[1];
      expect(block).toBe("  contents: read\n  id-token: write\n");
      expect(workflow.match(/permissions:/g)).toHaveLength(1);
    }
  });

  it("lists exactly its templates' digests in the minter's canonical set", () => {
    expect(minterDigests()).toEqual(githubBuildWorkflowDigests());
  });

  it("dispatches build ids that getployz/build accepts", () => {
    const accepted = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/;
    const deployment = "36755d9a-dc30-4385-a9bd-86ae1a85e44b";
    for (const service of ["a", "ployz-soak-node", "s".repeat(63)]) {
      expect(githubBuildId(deployment, service)).toMatch(accepted);
    }
  });

  it("opens GitHub's new-file page on the default branch with the workflow filled in", () => {
    const url = new URL(githubBuildWorkflowUrl({ web: "http://127.0.0.1:4555", fullName: "acme/api", defaultBranch: "release/v2" }));
    expect(url.origin + url.pathname).toBe("http://127.0.0.1:4555/acme/api/new/release/v2");
    expect(url.searchParams.get("filename")).toBe(".github/workflows/ployz-build.yml");
    expect(url.searchParams.get("value")).toBe(GITHUB_BUILD_WORKFLOW);
  });

  it("opens GitHub's editor on the existing workflow file", () => {
    expect(githubBuildWorkflowEditUrl({ web: "https://github.com", fullName: "acme/api", defaultBranch: "release/v2" }))
      .toBe("https://github.com/acme/api/edit/release/v2/.github/workflows/ployz-build.yml");
  });
});
