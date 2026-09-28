import { queryOptions } from "@tanstack/react-query";
import { isNotFound } from "#/lib/error-message";
import { teardownIsBusy, type TeardownScope } from "#/modules/runtime/teardown";
import { loadLatestTeardownAttemptServerFn } from "#/modules/runtime/teardown.functions";

export function latestTeardownAttemptQueryOptions(
  input: {
    organizationSlug: string;
    scope: TeardownScope;
    environmentId?: string;
    projectSlug?: string;
  },
) {
  return queryOptions({
    queryKey: [
      "teardown-attempt",
      input.organizationSlug,
      input.scope,
      input.environmentId ?? null,
      input.projectSlug ?? null,
    ] as const,
    queryFn: () => loadLatestTeardownAttemptServerFn({ data: input }),
    // Mount reads are authoritative; a busy attempt polls until terminal.
    staleTime: 0,
    refetchInterval: (query) => {
      const row = query.state.data;
      return row != null && teardownIsBusy(row.status) ? 2_000 : false;
    },
    // Not found means it's gone, which retrying won't change.
    retry: (failures, error) => !isNotFound(error) && failures < 3,
  });
}
