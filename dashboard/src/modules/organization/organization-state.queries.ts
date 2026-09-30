import { toast } from "sonner";
import { environmentManager, queryOptions, type QueryClient } from "@tanstack/react-query";
import { getOrganizationStateServerFn, syncOrganizationSlugServerFn } from "./organization-state.functions";

export const organizationKeys = {
  all: ["organization"] as const,
  state: (organizationSlug?: string | null) =>
    [...organizationKeys.all, organizationSlug ?? null, "state"] as const,
};

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
