import type { SupportAccessRow } from "./support-access";
import { defineOrgCollection } from "#/collections/change-log/declare";
import { storeViewPrefix } from "#/modules/config-store/store-view.queries";

/** Whether the Organization lets Ployz support open it: no row reads as off. */
export const supportAccess = defineOrgCollection({
  name: "organization_support_access",
  sources: ["organization_support_access"],
  key: (row: SupportAccessRow) => row.id,
  // What support may read changed: every Store view rereads, so support's cache drops what it may no longer see.
  invalidate: (organizationSlug, scope) => void scope.queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug) }, { cancelRefetch: false }),
});
