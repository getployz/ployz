import { infiniteQueryOptions } from "@tanstack/react-query";
import { listDeploymentOperationEvidenceServerFn } from "#/modules/deployments/deployment.functions";

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

const deploymentEvidenceKeys = {
  all: ["deployment-operation-evidence"] as const,
  page: (organizationSlug: string, deploymentId: string) =>
    [...deploymentEvidenceKeys.all, organizationSlug, deploymentId] as const,
};

export function deploymentOperationEvidenceQueryOptions(input: {
  organizationSlug: string;
  deploymentId: string;
  enabled: boolean;
}) {
  return infiniteQueryOptions({
    queryKey: deploymentEvidenceKeys.page(
      input.organizationSlug,
      input.deploymentId,
    ),
    queryFn: ({ pageParam, signal }) =>
      listDeploymentOperationEvidenceServerFn({
          data: pageParam
            ? {
                organizationSlug: input.organizationSlug,
                deploymentId: input.deploymentId,
                afterSequence: pageParam,
                limit: 50,
              }
            : {
                organizationSlug: input.organizationSlug,
                deploymentId: input.deploymentId,
                limit: 50,
              },
          signal,
        }),
    // SAFETY: TanStack infers PageParam from initialPageParam; later pages pass a sequence string.
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (lastPage) => lastPage?.nextSequence ?? undefined,
    enabled: input.enabled,
  });
}
