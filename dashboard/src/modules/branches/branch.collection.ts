import { useEffect, useState } from "react";
import { toast } from "sonner";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { getBranchesCollection, getEnvironmentDeploymentsCollection, getEnvironmentsCollection, getProjectsCollection } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { hasUndeployedChanges } from "#/modules/environment-design/environment-change-set";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import type { SetupCommand } from "#/modules/project/tables";
import { setBranchKeptServerFn, setBranchSetupDefaultsServerFn } from "./branch-functions";
import { idleClose } from "./idle-close";

const getKeepBranchAction = cachedByCollectionScope((organizationSlug, scope) => {
  const branches = getBranchesCollection(organizationSlug, scope);
  return createOptimisticAction<{ environmentId: string; kept: boolean }>({
    onMutate: ({ environmentId, kept }) => branches.update(environmentId, (draft) => { draft.kept = kept; }),
    mutationFn: async ({ environmentId, kept }) => {
      try {
        await branches.writeCommitted(await setBranchKeptServerFn({ data: { organizationSlug, environmentId, kept } }));
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not change whether this branch is kept.");
        throw error;
      }
    },
  });
});

/** Keeps a Branch, or stops keeping it; applies at once and rolls back on failure. */
export function useKeepBranch(organizationSlug: string) {
  const keep = getKeepBranchAction(organizationSlug, useCollectionScope());
  return (environmentId: string, kept: boolean) => observeFailure(keep({ environmentId, kept }));
}

/** Whether this Environment closes soon for sitting idle, by the same rule the hourly sweep uses. */
export function useIdleClose(organizationSlug: string, environmentId: string) {
  const scope = useCollectionScope();
  const { data: branches } = useLiveSuspenseQuery(getBranchesCollection(organizationSlug, scope));
  const { data: projects } = useLiveSuspenseQuery(getProjectsCollection(organizationSlug, scope));
  const { data: deployments } = useLiveSuspenseQuery(getEnvironmentDeploymentsCollection(organizationSlug, scope));
  // ponytail: the time moves hourly; the warning counts whole days, so an hour late doesn't matter.
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 60 * 60 * 1000);
    return () => clearInterval(timer);
  }, []);
  const latest = deployments
    .filter((deployment) => deployment.environmentId === environmentId)
    .reduce<Date | undefined>((at, deployment) => (at && at > deployment.createdAt ? at : deployment.createdAt), undefined);
  return idleClose(environmentId, {
    branches,
    latestAttemptAt: new Map(latest ? [[environmentId, latest]] : []),
    defaultEnvironmentIds: new Set(projects.flatMap((project) => (project.defaultEnvironmentId ? [project.defaultEnvironmentId] : []))),
  }, now);
}

/**
 * The Environment if it's a Branch with no Cloud Deployment Attempt yet: a starting point other Branches copy from. Root Environments never are.
 * The Org Store keeps each Environment's latest attempt, so having none means it was never deployed.
 */
export function useStartingPoint(organizationSlug: string, environmentId: string) {
  const { environments, branches } = useWorkspace(organizationSlug);
  const attempts = useEnvironmentDeployments(organizationSlug, environmentId);
  if (attempts.length > 0 || !branches.some((branch) => branch.environmentId === environmentId)) return undefined;
  return environments.find((environment) => environment.id === environmentId);
}

/** A PR Environment's last shutdown until it deploys again (`off`: Off, its rows kept), or null. */
export function useShutdown(organizationSlug: string, environmentId: string) {
  return useWorkspace(organizationSlug).branches.find((branch) => branch.environmentId === environmentId)?.pullRequest?.shutdown ?? null;
}

/** Whether anything is staged here: Working State differs from Applied State, as the server's gate reads it. */
function useHasStagedChanges(organizationSlug: string, environmentId: string) {
  const document = useEnvironmentDocument(organizationSlug, environmentId);
  const state = useEnvironmentChangeStateProjection({ organizationSlug, environmentId });
  return hasUndeployedChanges(document?.compiled.nodeSnapshots ?? [], state?.applied.nodes ?? []);
}

/**
 * Why Update and Own Copy must wait, or null: they rewrite what a Branch runs. Something staged (a starting point's nodes
 * too) or an active attempt holds them; the server's gate, assertBranchSettled, is the same rule. Save never waits.
 */
export function useBranchUnsettled(organizationSlug: string, environmentId: string): string | null {
  const startingPoint = useStartingPoint(organizationSlug, environmentId);
  const staged = useHasStagedChanges(organizationSlug, environmentId);
  const attempts = useEnvironmentDeployments(organizationSlug, environmentId);
  if (startingPoint) return "Deploy this starting point first.";
  if (attempts.some(({ deployment }) => isActiveDeployment(deployment.status))) return "Wait for this branch's deployment to finish.";
  return staged ? "Deploy or discard the changes staged here first." : null;
}

const getBranchSetupDefaultsAction = cachedByCollectionScope((organizationSlug, scope) => {
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  return createOptimisticAction<{ environmentId: string; setupCommands: SetupCommand[] }>({
    onMutate: ({ environmentId, setupCommands }) => environments.update(environmentId, (draft) => { draft.branchSetupCommands = setupCommands; }),
    mutationFn: async (data) => {
      try {
        await environments.writeCommitted(await setBranchSetupDefaultsServerFn({
          data: { organizationSlug, environmentId: data.environmentId, setupCommands: data.setupCommands },
        }));
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not save the setup commands.");
        throw error;
      }
    },
  });
});

/** Saves the Setup Commands that prefill new Branches of an Environment; applies at once and rolls back on failure. */
export function useBranchSetupDefaults(organizationSlug: string) {
  const save = getBranchSetupDefaultsAction(organizationSlug, useCollectionScope());
  return (environmentId: string, setupCommands: SetupCommand[]) => observeFailure(save({ environmentId, setupCommands }));
}
