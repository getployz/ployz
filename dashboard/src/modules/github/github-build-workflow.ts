/** Where the GitHub build workflow lives in a repository. Cloud dispatches it by this file name. */
export const GITHUB_BUILD_WORKFLOW_FILE = "ployz-build.yml";

/**
 * The workflow `getployz/build@v1` expects, byte for byte `ployz-build.yml` in https://github.com/getployz/build.
 * Cloud dispatches `build`, `cloud` and `runner`. The check-in names the ployz version to install.
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
      - uses: getployz/build@v1
        with:
          build: \${{ inputs.build }}
          cloud: \${{ inputs.cloud }}
`;

/**
 * Readiness of one repository for GitHub builds, as Cloud last checked it.
 * `waiting` (Add workflow opened, commit not seen yet) is only known to the browser that opened it.
 */
export type GithubBuildReadiness = "ready" | "setup_needed" | "no_permission";

export type GithubBuildRepository = {
  installationId: number;
  repositoryId: number;
  fullName: string;
  defaultBranch: string | null;
  readiness: GithubBuildReadiness;
};

export function githubBuildRepositoryKey(repository: Pick<GithubBuildRepository, "installationId" | "repositoryId">) {
  return `${repository.installationId}:${repository.repositoryId}`;
}

/** GitHub's new-file page with the workflow filled in. The user commits it or opens a PR; Cloud never writes. */
export function githubBuildWorkflowUrl(input: { fullName: string; defaultBranch: string }) {
  const params = new URLSearchParams({
    filename: `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`,
    value: GITHUB_BUILD_WORKFLOW,
  });
  const [owner = "", name = ""] = input.fullName.split("/");
  return `https://github.com/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/new/${input.defaultBranch.split("/").map(encodeURIComponent).join("/")}?${params.toString()}`;
}
