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
  const mutations = scope.queryClient.getMutationCache();
  const hasQueuedChanges = useSyncExternalStore(
    (onChange) => mutations.subscribe(onChange),
    () => mutations.findAll({
      status: "pending",
      predicate: (mutation) => mutation.state.isPaused
        && mutation.options.mutationKey?.[1] === organizationSlug
        && (mutation.options.mutationKey?.[0] === "store-edit" || mutation.options.mutationKey?.[0] === "store-command"),
    }).length > 0,
    () => false,
  );
  if (!hasError && !hasQueuedChanges) return null;
  return (
    <Alert>
      {hasQueuedChanges ? <AlertDescription>
        Changes are queued and not saved yet. Keep this tab open; saving resumes automatically.
      </AlertDescription> : null}
      {hasError ? <AlertDescription>
        Could not refresh organization data. Shown results may be out of date. Retrying automatically.
      </AlertDescription> : null}
    </Alert>
  );
}
