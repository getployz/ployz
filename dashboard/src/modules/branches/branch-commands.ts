import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import {
  getBranchesCollection,
  getCanvasPositionsCollection,
  getEnvironmentDeploymentsCollection,
  getEnvironmentNodeIntroductionsCollection,
  getEnvironmentsCollection,
  getEnvironmentSummariesCollection,
  getRawEnvironmentResourcesCollection,
  getRawServicesCollection,
} from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { refetchEnvironmentChangeStates } from "#/modules/deployments/environment-change-state.queries";
import { createBranchServerFn } from "./branch-functions";
import type { CreateBranch } from "./branch-schemas";

/** Every Org Store table a new Branch writes rows into. */
const branchTables = [
  getEnvironmentsCollection, getEnvironmentSummariesCollection, getBranchesCollection, getRawServicesCollection,
  getRawEnvironmentResourcesCollection, getCanvasPositionsCollection, getEnvironmentNodeIntroductionsCollection,
  getEnvironmentDeploymentsCollection,
];

/**
 * Creates a Branch and its first deployment, then opens its canvas. Awaited: the server assigns the Branch's ids.
 * The Branch's rows are read back before navigating so its canvas opens complete.
 */
export function useCreateBranch(projectSlug: string) {
  const scope = useCollectionScope();
  const navigate = useNavigate();
  return useMutation({
    mutationFn: async (input: CreateBranch) => {
      const { data } = await createBranchServerFn({ data: input });
      await Promise.all(branchTables.map((get) => reconcileCollection(get(input.organizationSlug, scope))));
      refetchEnvironmentChangeStates(input.organizationSlug, scope);
      return data;
    },
    onSuccess: (data, input) => navigate(getDashboardDestination({
      kind: "environment", organizationSlug: input.organizationSlug, projectSlug, environmentSlug: data.environment.namespace,
    }, "canvas")),
  });
}
