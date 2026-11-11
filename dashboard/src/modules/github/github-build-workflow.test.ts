import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { githubWorkflowTemplates } from "./github-authority.server";
import {
  GITHUB_BUILD_WORKFLOW,
  GITHUB_BUILDER_COMMIT,
  GITHUB_RULE_WORKFLOW,
  GITHUB_WORKFLOW_TEMPLATES,
  githubBuildId,
  githubBuildWorkflowEditUrl,
  githubBuildWorkflowUrl,
} from "./github-build-workflow";

/** A file of this repository, read from disk: the dashboard never imports from `minter/` or `core/`. */
const repositoryFile = (path: string) => readFileSync(new URL(`../../../../${path}`, import.meta.url), "utf8");

const minterDigests = () => new Set((JSON.parse(repositoryFile("minter/src/canonical-workflows.json")) as { digests: string[] }).digests);

const uses = (workflow: string) => [...workflow.matchAll(/^\s*(?:-\s+)?uses:\s*(\S+)\s*$/gm)].map((found) => found[1]);

describe("GitHub build workflows", () => {
  it("calls the builder at a full commit with every input Cloud dispatches", () => {
    expect(uses(GITHUB_BUILD_WORKFLOW)).toEqual([`getployz/build@${GITHUB_BUILDER_COMMIT}`]);
    expect(GITHUB_BUILDER_COMMIT).toMatch(/^[0-9a-f]{40}$/);
    for (const input of ["build", "cloud"]) {
      expect(GITHUB_BUILD_WORKFLOW).toContain(`${input}: \${{ inputs.${input} }}`);
    }
  });

  it("calls the reusable builder each Server checks a rule build ran", () => {
    expect(uses(GITHUB_RULE_WORKFLOW)).toEqual([`getployz/build/.github/workflows/build.yml@${GITHUB_BUILDER_COMMIT}`]);
    const pinned = /pub const BUILDER_SHA: &str = "([0-9a-f]{40})";/.exec(repositoryFile("core/crates/ployz-core/src/authority/rule.rs"))?.[1];
    expect(GITHUB_BUILDER_COMMIT).toBe(pinned);
  });

  it("names no branch or Cloud of a customer in the rule template", () => {
    expect(GITHUB_RULE_WORKFLOW).toMatch(/^ {2}push:\n {2}workflow_dispatch:\n/m);
    expect(GITHUB_RULE_WORKFLOW).toContain("cloud: ${{ inputs.cloud || vars.PLOYZ_CLOUD || 'https://ployz.dev' }}");
    expect(GITHUB_RULE_WORKFLOW).toContain("commit: ${{ inputs.commit }}");
  });

  it("gives the run's GITHUB_TOKEN only contents: read and id-token: write", () => {
    for (const workflow of Object.values(GITHUB_WORKFLOW_TEMPLATES)) {
      const block = /^permissions:\n((?: {2}\S.*\n)+)/m.exec(workflow)?.[1];
      expect(block).toBe("  contents: read\n  id-token: write\n");
      expect(workflow.match(/permissions:/g)).toHaveLength(1);
    }
  });

  it("lists exactly its templates' digests in the minter's canonical set", () => {
    const templates = githubWorkflowTemplates();
    expect(minterDigests()).toEqual(new Set(templates.keys()));
    expect(new Set(templates.values())).toEqual(new Set(["build", "rule"]));
  });

  it("shows the rule template verbatim where the docs give it", () => {
    for (const page of ["docs/user/deploy/github.md", "docs/user/environments/preview-environments.md"]) {
      expect(repositoryFile(page)).toContain(`\`\`\`yaml\n${GITHUB_RULE_WORKFLOW}\`\`\``);
    }
  });

  it("dispatches build ids that getployz/build accepts", () => {
    const accepted = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/;
    const deployment = "36755d9a-dc30-4385-a9bd-86ae1a85e44b";
    for (const service of ["a", "ployz-soak-node", "s".repeat(63)]) {
      expect(githubBuildId(deployment, service)).toMatch(accepted);
    }
  });

  it("opens GitHub's new-file page on the default branch with the offered workflow filled in", () => {
    for (const template of ["build", "rule"] as const) {
      const url = new URL(githubBuildWorkflowUrl({ web: "http://127.0.0.1:4555", fullName: "acme/api", defaultBranch: "release/v2", template }));
      expect(url.origin + url.pathname).toBe("http://127.0.0.1:4555/acme/api/new/release/v2");
      expect(url.searchParams.get("filename")).toBe(".github/workflows/ployz-build.yml");
      expect(url.searchParams.get("value")).toBe(GITHUB_WORKFLOW_TEMPLATES[template]);
    }
  });

  it("opens GitHub's editor on the existing workflow file", () => {
    expect(githubBuildWorkflowEditUrl({ web: "https://github.com", fullName: "acme/api", defaultBranch: "release/v2" }))
      .toBe("https://github.com/acme/api/edit/release/v2/.github/workflows/ployz-build.yml");
  });
});
