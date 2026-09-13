export const serviceDeploymentKeys = {
  all: ["service-deployments"] as const,
  org: (organizationSlug: string) =>
    [...serviceDeploymentKeys.all, organizationSlug] as const,
  environmentChangeStatesOrg: (organizationSlug: string) =>
    [
      ...serviceDeploymentKeys.org(organizationSlug),
      "environment-change-states",
    ] as const,
  listOrg: (organizationSlug: string) =>
    [...serviceDeploymentKeys.org(organizationSlug), "list"] as const,
};
