import { queryOptions } from "@tanstack/react-query";
import { teardownIsBusy, type TeardownScope } from "#/modules/runtime/teardown";
import type { loadLatestTeardownAttemptServerFn } from "#/modules/runtime/teardown.functions";

type LatestAttempt = Awaited<ReturnType<typeof loadLatestTeardownAttemptServerFn>>;

export function latestTeardownAttemptQueryOptions(
  input: {
    organizationSlug: string;
    scope: TeardownScope;
    environmentId?: string;
    projectSlug?: string;
  },
  read: () => Promise<LatestAttempt>,
) {
  return queryOptions({
    queryKey: [
      "teardown-attempt",
      input.organizationSlug,
      input.scope,
      input.environmentId ?? null,
      input.projectSlug ?? null,
    ] as const,
    queryFn: read,
    refetchInterval: (query) => {
      const row = query.state.data;
      return row != null && teardownIsBusy(row.status) ? 2_000 : false;
    },
  });
}
