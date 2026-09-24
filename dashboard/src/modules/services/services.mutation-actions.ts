import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import {
  clearServiceRegistryCredentialServerFn,
  restoreServiceRegistryCredentialServerFn,
  setServiceRegistryCredentialServerFn,
} from "#/modules/environment-design/service-functions";
import { reconcileCollection } from "#/collections/query-collection";
import { getRawServicesCollection } from "#/collections/collections";

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
  const editDocument = useEnvironmentDocumentEditor(organizationSlug);
  type CredentialAction = { kind: "clear" | "restore" } | { kind: "set"; username: string | null; secret: string };
  function edit(action: CredentialAction) {
    return editDocument({
      environmentId,
      apply: (intent) => {
        const node = intent.services.find((node) => node.id === serviceId);
        if (!node || node.config.source.type !== "image") return;
        node.config.source.credentials = action.kind === "clear"
          ? { type: "none" } : { type: "configured", credentialId: serviceId };
      },
      save: (revision) => {
        const data = { organizationSlug, environmentId, serviceId, revision };
        return action.kind === "set"
          ? setServiceRegistryCredentialServerFn({ data: { ...data, username: action.username ?? undefined, secret: action.secret } })
          : action.kind === "clear" ? clearServiceRegistryCredentialServerFn({ data })
            : restoreServiceRegistryCredentialServerFn({ data });
      },
      failureMessage: "Could not save registry credentials.",
      afterSave: async () => {
        await reconcileCollection(getRawServicesCollection(organizationSlug, collectionScope));
        onSuccess?.();
      },
    });
  }
  return {
    setCredentialAction: (input: { username: string | null; secret: string }) => edit({ kind: "set", ...input }),
    clearCredentialAction: () => edit({ kind: "clear" }),
    restoreCredentialAction: () => edit({ kind: "restore" }),
  };
}
