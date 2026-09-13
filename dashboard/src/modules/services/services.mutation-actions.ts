import { useCollectionScope } from "#/collections/use-collection-scope";
import { createOptimisticAction } from "@tanstack/react-db";
import {
  clearServiceRegistryCredentialServerFn,
  restoreServiceRegistryCredentialServerFn,
  setServiceRegistryCredentialServerFn,
} from "#/modules/environment-design/service-functions";
import { getEnvironmentsCollection } from "#/collections/collections";

type UseServiceRegistryCredentialActionsInput = {
  organizationSlug: string;
  environmentId: string;
  serviceId: string;
  onSuccess?: () => void;
};

export function useServiceRegistryCredentialActions({
  organizationSlug, environmentId, serviceId, onSuccess,
}: UseServiceRegistryCredentialActionsInput) {
  const collectionScope = useCollectionScope();
  const environments = getEnvironmentsCollection(organizationSlug, collectionScope);
  type CredentialAction = { kind: "clear" | "restore" } | { kind: "set"; username: string | null; secret: string };
  const persist = createOptimisticAction<{ action: CredentialAction; revision: string }>({
    onMutate: ({ action }) => {
      environments.update(environmentId, (draft) => {
        const node = draft.intent.services.find((node) => node.id === serviceId);
        if (!node || node.config.source.type !== "image") throw new Error("Service does not use a container image.");
        node.config.source.credentials = action.kind === "clear"
          ? { type: "none" } : { type: "configured", revision: new Date().toISOString() };
      });
    },
    mutationFn: async ({ action, revision }) => {
      const data = { organizationSlug, environmentId, serviceId, revision };
      const result = await (action.kind === "set"
        ? setServiceRegistryCredentialServerFn({ data: { ...data, username: action.username ?? undefined, secret: action.secret } })
        : action.kind === "clear" ? clearServiceRegistryCredentialServerFn({ data })
          : restoreServiceRegistryCredentialServerFn({ data }));
      await environments.writeCommitted(result.data);
      onSuccess?.();
    },
  });
  function edit(action: CredentialAction) {
    const document = environments.get(environmentId);
    if (!document) throw new Error("Environment is not loaded.");
    return persist({ action, revision: document.revision });
  }
  return {
    setCredentialAction: (input: { username: string | null; secret: string }) => edit({ kind: "set", ...input }),
    clearCredentialAction: () => edit({ kind: "clear" }),
    restoreCredentialAction: () => edit({ kind: "restore" }),
  };
}
