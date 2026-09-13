export function getGitRepoSelectorState(input: {
  configured: boolean;
  hasInstallations: boolean;
  repoCount: number;
  filteredRepoCount: number;
}) {
  if (!input.configured) {
    return "not-configured" as const;
  }

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
