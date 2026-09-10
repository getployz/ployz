import { useSyncExternalStore } from "react";
import type { CollectionScope } from "#/collections/scope";
import { Alert, AlertDescription } from "#/components/ui/alert";

export function OrganizationCollectionRefreshNotice({
  scope,
  organizationSlug,
}: { scope: CollectionScope; organizationSlug: string }) {
  const cache = scope.queryClient.getQueryCache();
  // Failed refreshes retain rows, so observe Query state rather than collection changes.
  const hasError = useSyncExternalStore(
    (onChange) => cache.subscribe(onChange),
    () => cache.findAll({
      queryKey: ["collections", scope.sessionId, scope.userId, organizationSlug],
      type: "active",
      predicate: (query) => query.queryKey.length === 5 && query.state.status === "error",
    }).length > 0,
    () => false,
  );
  if (!hasError) return null;
  return (
    <Alert>
      <AlertDescription>
        Could not refresh organization data. Shown results may be out of date. Retrying automatically.
      </AlertDescription>
    </Alert>
  );
}
