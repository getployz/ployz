import { useState } from "react";
import { toast } from "sonner";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { getBranchesCollection, getEnvironmentDeploymentsCollection, getProjectsCollection } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { setBranchKeptServerFn } from "./branch-close.functions";
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
  // ponytail: read once per mount; the warning moves in whole days, so a stale hour doesn't matter.
  const [now] = useState(() => new Date());
  const latest = deployments
    .filter((deployment) => deployment.environmentId === environmentId)
    .reduce<Date | undefined>((at, deployment) => (at && at > deployment.createdAt ? at : deployment.createdAt), undefined);
  return idleClose(environmentId, {
    branches,
    latestAttemptAt: new Map(latest ? [[environmentId, latest]] : []),
    defaultEnvironmentIds: new Set(projects.flatMap((project) => (project.defaultEnvironmentId ? [project.defaultEnvironmentId] : []))),
  }, now);
}
