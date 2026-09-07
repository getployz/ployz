import { createOptimisticAction } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import {
  clearServiceRegistryCredentialServerFn,
  restoreServiceRegistryCredentialServerFn,
  setServiceRegistryCredentialServerFn,
} from "#/modules/environment-design/service-functions";
import { getRawServicesCollection } from "#/electric/collections";
import { useServicesCollection } from "#/modules/services/services.collection";

type UseServiceRegistryCredentialActionsInput = {
  organizationSlug: string;
  environmentId: string;
  serviceId: string;
  onSuccess?: () => void;
};

export function useServiceRegistryCredentialActions({
  organizationSlug,
  environmentId,
  serviceId,
  onSuccess,
}: UseServiceRegistryCredentialActionsInput) {
  const collection = useServicesCollection(organizationSlug);
  const rawServices = getRawServicesCollection(organizationSlug);
  const setRegistryCredential = useServerFn(
    setServiceRegistryCredentialServerFn,
  );
  const clearRegistryCredential = useServerFn(
    clearServiceRegistryCredentialServerFn,
  );
  const restoreRegistryCredential = useServerFn(
    restoreServiceRegistryCredentialServerFn,
  );

  const setCredentialAction = createOptimisticAction<{
    username: string | null;
    secret: string;
  }>({
    onMutate: ({ username }) => {
      const revision = new Date().toISOString();

      collection.update(serviceId, (draft) => {
        if (draft.source.type !== "image") {
          return;
        }

        draft.source.credentials = {
          type: "configured",
          revision,
        };
        draft.registryCredentialUsername = username;
        draft.hasStoredRegistryCredential = true;
      });
    },
    mutationFn: async ({ username, secret }) => {
      const receipt = await setRegistryCredential({
        data: {
          organizationSlug,
          environmentId,
          serviceId,
          username: username ?? undefined,
          secret,
        },
      });
      await rawServices.utils.awaitTxId(receipt.txid);
      onSuccess?.();
    },
  });

  const clearCredentialAction = createOptimisticAction<void>({
    onMutate: () => {
      collection.update(serviceId, (draft) => {
        if (draft.source.type !== "image") {
          return;
        }

        draft.source.credentials = {
          type: "none",
        };
      });
    },
    mutationFn: async () => {
      const receipt = await clearRegistryCredential({
        data: { organizationSlug, environmentId, serviceId },
      });
      await rawServices.utils.awaitTxId(receipt.txid);
      onSuccess?.();
    },
  });

  const restoreCredentialAction = createOptimisticAction<void>({
    onMutate: () => {
      const revision = new Date().toISOString();

      collection.update(serviceId, (draft) => {
        if (draft.source.type !== "image") {
          return;
        }

        draft.source.credentials = {
          type: "configured",
          revision,
        };
      });
    },
    mutationFn: async () => {
      const receipt = await restoreRegistryCredential({
        data: { organizationSlug, environmentId, serviceId },
      });
      await rawServices.utils.awaitTxId(receipt.txid);
      onSuccess?.();
    },
  });

  return {
    setCredentialAction,
    clearCredentialAction,
    restoreCredentialAction,
  };
}
