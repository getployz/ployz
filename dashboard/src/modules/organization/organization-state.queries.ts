/** @data-source remote: organization state: fresh on every mount; the change stream invalidates it when the organization changes */
import { toast } from "sonner";
import { environmentManager, queryOptions, useMutation, useQueryClient, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { getOrganizationStateServerFn, renameOrganizationServerFn, syncOrganizationSlugServerFn } from "./organization-state.functions";
import { defineChange } from "#/collections/change-log/declare";

export const organizationKeys = {
  all: ["organization"] as const,
  state: (organizationSlug?: string | null) =>
    [...organizationKeys.all, organizationSlug ?? null, "state"] as const,
};

export const organizationChange = defineChange({
  name: "organization",
  sources: ["organization"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: organizationKeys.all }),
});

export function organizationStateQueryOptions(organizationSlug?: string | null) {
  return queryOptions({
    queryKey: organizationKeys.state(organizationSlug),
    // Membership and the active organization are authoritative on every mount.
    staleTime: 0,
    queryFn: ({ signal }) =>
      getOrganizationStateServerFn({
        data: organizationSlug === undefined || organizationSlug === null
          ? {}
          : { organizationSlug },
        signal,
      }),
  });
}

/** An Organization's name, current after a rename here or in another tab. */
export function useOrganizationName(organizationSlug: string) {
  return useSuspenseQuery({
    ...organizationStateQueryOptions(organizationSlug),
    select: (state) => state.organizations.find((organization) => organization.slug === organizationSlug)?.name ?? organizationSlug,
  }).data;
}

/** Renames the Organization, and refetches its state once the rename is saved. */
export function useRenameOrganization() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: useServerFn(renameOrganizationServerFn),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: organizationKeys.all }),
  });
}

/** Route lifecycle runs on the server too. Only committed browser navigation stores preferences. */
export async function rememberSelectedOrganization(client: QueryClient, slug: string, initialSlug: string | null, selectOrganization = syncOrganizationSlugServerFn) {
  if (environmentManager.isServer()) return;
  const key = ["organization-preference"];
  try {
    await client.getMutationCache().build(client, {
      scope: { id: "organization-preference" },
      mutationFn: async () => {
        if ((client.getQueryData<string>(key) ?? initialSlug) === slug) return;
        await selectOrganization({ data: { organizationSlug: slug } });
        client.setQueryData(key, slug);
      },
    }).execute(undefined);
  } catch {
    toast.error("Could not remember your selected organization.");
  }
}
