import type { EnvironmentRef } from "@ployz/sdk";
import { environmentsQuery, useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { useYouMay } from "#/modules/organization/members.queries";

/**
 * Whether a write into `environment` only asks: it is Protected and you can't protect it, so an owner or admin
 * approves it first. False until the listing and your role load; the Store refuses either way.
 */
export function useProtectedAsk(organizationSlug: string, environment: EnvironmentRef) {
  const listing = useCachedStoreView(organizationSlug, environmentsQuery(environment.project ?? ""));
  const mayProtect = useYouMay(organizationSlug, "environments:protect");
  const current = listing?.ok
    ? listing.value.environments.find((row) => environment.environment == null ? row.default : row.name === environment.environment)
    : undefined;
  return current?.protected === true && mayProtect === false;
}
