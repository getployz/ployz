import { toast } from "sonner";
import { createOptimisticAction } from "@tanstack/react-db";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRawServicesCollection } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { editServiceMetadataServerFn } from "./service-metadata.functions";
import type { ServiceMetadataEdit } from "./service-metadata";

const getServiceMetadataEditor = cachedByCollectionScope((organizationSlug, scope) => {
  const identities = getRawServicesCollection(organizationSlug, scope);
  return createOptimisticAction<Omit<ServiceMetadataEdit, "organizationSlug">>({
    onMutate: ({ serviceId, edit }) => identities.update(serviceId, draft => {
      if (edit.kind === "rename") draft.name = edit.name;
      else {
        const { preferredBuilder, ...policy } = structuredClone(edit.policy);
        Object.assign(draft.policy, policy);
        if (preferredBuilder === null) delete draft.policy.preferredBuilder;
        else if (preferredBuilder !== undefined) draft.policy.preferredBuilder = preferredBuilder;
      }
    }),
    mutationFn: async (input) => {
      try {
        const result = await editServiceMetadataServerFn({ data: { ...input, organizationSlug } });
        await identities.writeCommitted(result.data);
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not save this setting.");
        throw error;
      }
    },
  });
});

export function useServiceMetadataEditor(organizationSlug: string) {
  const edit = getServiceMetadataEditor(organizationSlug, useCollectionScope());
  return (input: Parameters<typeof edit>[0]) => observeFailure(edit(input));
}
