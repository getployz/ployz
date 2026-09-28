import { useMutation } from "@tanstack/react-query";
import { toast } from "sonner";
import { getBranchesCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { reconcileDeploymentCollections } from "#/modules/deployments/deployment.collection";
import { shutDownPrEnvironmentServerFn, startPrEnvironmentServerFn } from "./off-functions";

/**
 * Shut down: the PR Environment's shutdown runs, then it's Off, its rows kept. Deploy: it starts again. Both wait on the
 * server.
 */
export function usePrEnvironmentOff(input: { organizationSlug: string; environmentId: string; name: string }) {
  const scope = useCollectionScope();
  const { name, ...data } = input;
  const settle = () => Promise.all([
    reconcileCollection(getBranchesCollection(input.organizationSlug, scope)),
    reconcileDeploymentCollections(input.organizationSlug, scope),
  ]);
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
