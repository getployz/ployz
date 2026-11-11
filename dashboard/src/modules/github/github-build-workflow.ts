/** Where the GitHub build workflow lives in a repository. Cloud dispatches it by this file name. */
export const GITHUB_BUILD_WORKFLOW_FILE = "ployz-build.yml";

/** The `getployz/build` commit every dispatched build runs. A tag can move; a commit can't. */
export const GITHUB_BUILDER_COMMIT = "a19b4fca3adea371c25ec29f181e8c7d21037d1e";

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

export function githubBuildId(deploymentId: string, service: string) {
  return `${deploymentId}.${service}`;
}

/** Every `ployz-build.yml` Ployz dispatches, byte for byte up to line endings. */
export const GITHUB_BUILD_WORKFLOWS: readonly string[] = [GITHUB_BUILD_WORKFLOW];

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
export function githubBuildWorkflowUrl(input: GithubBuildWorkflowPage) {
  const params = new URLSearchParams({
    filename: `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`,
    value: GITHUB_BUILD_WORKFLOW,
  });
  return `${repositoryPage(input, "new")}?${params.toString()}`;
}

/** GitHub's editor on the repository's existing workflow file. The person pastes the new file there. */
export function githubBuildWorkflowEditUrl(input: GithubBuildWorkflowPage) {
  return `${repositoryPage(input, "edit")}/.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`;
}
