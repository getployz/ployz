import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { volumeRemoveIsBusy } from "#/modules/runtime/volume-removal";
import type { loadLatestVolumeRemoveAttemptServerFn } from "#/modules/runtime/volume-removal.functions";

type LatestAttempt = Awaited<
  ReturnType<typeof loadLatestVolumeRemoveAttemptServerFn>
>;

export type LatestVolumeRemoveAttempt = NonNullable<LatestAttempt>;

export function latestVolumeRemoveAttemptQueryOptions(
  input: {
    organizationSlug: string;
    environmentId: string;
    resourceId: string;
  },
  read: () => Promise<LatestAttempt>,
) {
  return queryOptions({
    queryKey: [
      "volume-remove-attempt",
      input.organizationSlug,
      input.environmentId,
      input.resourceId,
    ] as const,
    queryFn: read,
    refetchInterval: (query) => {
      const row = query.state.data;
      return row != null && volumeRemoveIsBusy(row.status) ? 2_000 : false;
    },
  });
}

type LatestQueryKey = ReturnType<
  typeof latestVolumeRemoveAttemptQueryOptions
>["queryKey"];

export async function rememberLatestVolumeRemoveAttempt(
  queryClient: QueryClient,
  queryKey: LatestQueryKey,
  mutate: () => Promise<LatestVolumeRemoveAttempt>,
): Promise<LatestVolumeRemoveAttempt> {
  try {
    const next = await mutate();
    queryClient.setQueryData(queryKey, next);
    return next;
  } catch (error) {
    await queryClient.invalidateQueries({ queryKey });
    throw error;
  }
}
