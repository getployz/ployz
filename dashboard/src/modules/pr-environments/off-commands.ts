import { useMutation } from "@tanstack/react-query";
import { toast } from "sonner";
import { getBranchesCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { CollectionScope } from "#/collections/scope";
import { reconcileDeploymentCollections } from "#/modules/deployments/deployment.collection";
import { shutDownPrEnvironmentServerFn, startPrEnvironmentServerFn } from "./off-functions";

/** Reads back what a shutdown or a deploy changes: the Branch rows (their shutdown state) and the attempts. */
export const reconcileOff = (organizationSlug: string, scope: CollectionScope) => Promise.all([
  reconcileCollection(getBranchesCollection(organizationSlug, scope)),
  reconcileDeploymentCollections(organizationSlug, scope),
]);

/**
 * Shut down: the PR Environment's shutdown runs, then it's Off, its rows kept. Deploy: it starts again. Both wait on the
 * server.
 */
export function usePrEnvironmentOff(input: { organizationSlug: string; environmentId: string; name: string }) {
  const scope = useCollectionScope();
  const { name, ...data } = input;
  const settle = () => reconcileOff(input.organizationSlug, scope);
  return {
    shutDown: useMutation({
      mutationFn: async () => {
        await shutDownPrEnvironmentServerFn({ data });
        await settle();
      },
      onSuccess: () => toast.success(`Shutting down ${name}`, { description: "Starts again on the next push" }),
      onError: (error) => toast.error(error.message),
    }),
    start: useMutation({
      mutationFn: async () => {
        await startPrEnvironmentServerFn({ data });
        await settle();
      },
      onError: (error) => toast.error(error.message),
    }),
  };
}
