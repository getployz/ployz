export function getGitRepoSelectorState(input: {
  hasInstallations: boolean;
  repoCount: number;
  filteredRepoCount: number;
}) {
  if (!input.hasInstallations) {
    return "no-installations" as const;
  }

  if (input.repoCount === 0) {
    return "empty" as const;
  }

  if (input.filteredRepoCount === 0) {
    return "no-results" as const;
  }

  return "ready" as const;
}
