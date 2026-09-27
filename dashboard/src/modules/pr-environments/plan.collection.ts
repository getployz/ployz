import { toast } from "sonner";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { getEnvironmentsCollection, getPrEnvironmentPlansCollection, prEnvironmentPlanKey, type PrEnvironmentPlanRow } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { VirtualRowProps } from "@tanstack/react-db";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import type { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { planRepositories } from "./repositories";
import { setPrEnvironmentPlanServerFn } from "./plan-functions";

type Project = ReturnType<typeof useWorkspace>["projects"][number];

/** Every GitHub repository the project's services deploy from, each with its plan: the saved one, or Off from the Default Environment. */
export function usePrEnvironmentPlans(organizationSlug: string, project: Project) {
  const scope = useCollectionScope();
  const { data: environments } = useLiveSuspenseQuery(getEnvironmentsCollection(organizationSlug, scope));
  const { data } = useLiveSuspenseQuery(getPrEnvironmentPlansCollection(organizationSlug, scope));
  // SAFETY: live-query rows carry TanStack's four virtual props at runtime, which the row type leaves out.
  const saved = new Map(data.map((row) => [prEnvironmentPlanKey(row), withoutVirtualProps(row as VirtualRowProps & typeof row)]));
  return planRepositories(environments.filter((environment) => environment.projectId === project.id)).map((repository): PrEnvironmentPlanRow =>
    saved.get(prEnvironmentPlanKey({ projectId: project.id, repositoryId: repository.repositoryId })) ?? {
      ...repository, organizationId: project.organizationId, projectId: project.id, enabled: false,
      startFromEnvironmentId: project.resolvedEnvironment?.id ?? null, picks: { preset: "only" }, setupCommands: [],
      removeOnClose: true, includeBots: false, enabledByUserId: null, updatedAt: new Date(0),
    });
}

const getPlanAction = cachedByCollectionScope((organizationSlug, scope) => {
  const plans = getPrEnvironmentPlansCollection(organizationSlug, scope);
  return createOptimisticAction<{ projectSlug: string; plan: PrEnvironmentPlanRow }>({
    onMutate: ({ plan }) => {
      const key = prEnvironmentPlanKey(plan);
      if (plans.has(key)) plans.update(key, (draft) => Object.assign(draft, plan));
      else plans.insert(plan);
    },
    mutationFn: async ({ projectSlug, plan }) => {
      try {
        await plans.writeCommitted(await setPrEnvironmentPlanServerFn({ data: {
          organizationSlug, projectSlug, repositoryId: plan.repositoryId, enabled: plan.enabled,
          startFromEnvironmentId: plan.startFromEnvironmentId, picks: plan.picks, setupCommands: plan.setupCommands,
          removeOnClose: plan.removeOnClose, includeBots: plan.includeBots,
        } }));
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not save the PR environments plan.");
        throw error;
      }
    },
  });
});

/** Saves a change to one repository's plan; applies at once and rolls back on failure. */
export function useSetPrEnvironmentPlan(organizationSlug: string, projectSlug: string) {
  const save = getPlanAction(organizationSlug, useCollectionScope());
  return (plan: PrEnvironmentPlanRow, change: Partial<Pick<PrEnvironmentPlanRow,
    "enabled" | "startFromEnvironmentId" | "picks" | "removeOnClose" | "includeBots">>) =>
    observeFailure(save({ projectSlug, plan: { ...plan, ...change } }));
}
