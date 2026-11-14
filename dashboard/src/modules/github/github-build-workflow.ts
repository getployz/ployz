/** Where the GitHub build workflow lives in a repository. Cloud dispatches it by this file name. */
export const GITHUB_BUILD_WORKFLOW_FILE = "ployz-build.yml";

/**
 * The `getployz/build` commit every build runs, its action or its reusable workflow. A tag can move; a commit can't.
 * Each Server checks a rule build ran this commit's reusable workflow, so it is also ployz-core's `BUILDER_SHA`.
 */
export const GITHUB_BUILDER_COMMIT = "39e1f488e492c0641e520777e481e8f5b266985b";

/**
 * The workflow Ployz offers. Cloud dispatches `build`, `cloud` and `runner`, and the run's `GITHUB_TOKEN` only reads.
 * The check-in names the ployz version to install. The minter dispatches only a file whose digest is canonical, so a
 * change here changes `minter/src/canonical-workflows.json` with it.
 */
export const GITHUB_BUILD_WORKFLOW = `name: Ployz build
on:
  workflow_dispatch:
    inputs:
      build: { required: true, type: string }
      cloud: { required: true, type: string }
      runner: { required: false, type: string, default: ubuntu-latest }
permissions:
  contents: read
  id-token: write
jobs:
  build:
    runs-on: \${{ inputs.runner }}
    steps:
      - uses: getployz/build@${GITHUB_BUILDER_COMMIT}
        with:
          build: \${{ inputs.build }}
          cloud: \${{ inputs.cloud }}
`;

/**
 * The workflow a repository under an automatic deploy rule or a preview rule runs. A push to any branch, not a tag,
 * builds, since a preview builds on its own push run, and each Server checks the branch against the rule. A dispatch builds an
 * approved or reviewed `commit`. No file names a customer's branch or Cloud: on a dispatch the minter fills `cloud`;
 * on a push it comes from the repository's `PLOYZ_CLOUD` variable, which only the repository's admins can set.
 * Setting `PLOYZ_BRANCH` skips pushes to other branches.
 */
export const GITHUB_RULE_WORKFLOW = `name: Ployz build
on:
  push:
    branches: ['**']
  workflow_dispatch:
    inputs:
      cloud: { required: false, type: string }
      commit: { required: false, type: string }
permissions:
  contents: read
  id-token: write
jobs:
  build:
    if: github.event_name != 'push' || vars.PLOYZ_BRANCH == '' || github.ref_name == vars.PLOYZ_BRANCH
    uses: getployz/build/.github/workflows/build.yml@${GITHUB_BUILDER_COMMIT}
    with:
      cloud: \${{ inputs.cloud || vars.PLOYZ_CLOUD || 'https://ployz.dev' }}
      commit: \${{ inputs.commit }}
`;

/** Every `ployz-build.yml` Ployz offers and dispatches, byte for byte up to line endings. */
export const GITHUB_WORKFLOW_TEMPLATES = { build: GITHUB_BUILD_WORKFLOW, rule: GITHUB_RULE_WORKFLOW } as const;

/** Which of Ployz's workflows a repository needs: `rule` when pushes build under a rule. */
export type GithubWorkflowTemplate = keyof typeof GITHUB_WORKFLOW_TEMPLATES;

export function githubBuildId(deploymentId: string, service: string) {
  return `${deploymentId}.${service}`;
}

/**
 * Readiness of one repository for GitHub builds, as Cloud last checked it. `outdated`: the default branch holds a
 * `ployz-build.yml` that is not one Ployz offers, such as one calling `getployz/build@v1`.
 * `waiting` (the editor opened, commit not seen yet) is only known to the browser that opened it.
 */
export type GithubBuildReadiness = "ready" | "outdated" | "setup_needed" | "no_permission";

export type GithubBuildRepository = {
  installationId: number;
  repositoryId: number;
  fullName: string;
  defaultBranch: string | null;
  readiness: GithubBuildReadiness;
  /** The file Ployz offers this repository. */
  template: GithubWorkflowTemplate;
  /** The one branch the repository's automatic deploy rules build, which `PLOYZ_BRANCH` may name; else null. */
  ruleBranch: string | null;
};

/** Whether the person can still commit Ployz's workflow here: the browser that sent them to GitHub waits for it. */
export function awaitsWorkflowCommit(repository: Pick<GithubBuildRepository, "readiness">) {
  return repository.readiness === "setup_needed" || repository.readiness === "outdated";
}

export function githubBuildRepositoryKey(repository: Pick<GithubBuildRepository, "installationId" | "repositoryId">) {
  return `${repository.installationId}:${repository.repositoryId}`;
}

type GithubBuildWorkflowPage = { web: string; fullName: string; defaultBranch: string };

function repositoryPage(input: GithubBuildWorkflowPage, page: "new" | "edit") {
  const [owner = "", name = ""] = input.fullName.split("/");
  return `${input.web}/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/${page}/${input.defaultBranch.split("/").map(encodeURIComponent).join("/")}`;
}

/** GitHub's new-file page with the workflow filled in. The user commits it or opens a PR; Cloud never writes. */
export function githubBuildWorkflowUrl(input: GithubBuildWorkflowPage & { template: GithubWorkflowTemplate }) {
  const params = new URLSearchParams({
    filename: `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`,
    value: GITHUB_WORKFLOW_TEMPLATES[input.template],
  });
  return `${repositoryPage(input, "new")}?${params.toString()}`;
}

/** GitHub's editor on the repository's existing workflow file. The person pastes the new file there. */
export function githubBuildWorkflowEditUrl(input: GithubBuildWorkflowPage) {
  return `${repositoryPage(input, "edit")}/.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`;
}
