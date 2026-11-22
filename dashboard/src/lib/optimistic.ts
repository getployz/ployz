import type { QueryClient, QueryKey } from "@tanstack/react-query";

export type Snapshot<Data> = { previous: Data | undefined };

/**
 * `onMutate` and `rollback` for a mutation that edits one cached read at once: it stops reads in flight, keeps what was
 * cached and writes `apply`'s result. A failed save passes `onMutate`'s context to `rollback`.
 */
export function optimisticCache<Data, Input>(queryClient: QueryClient, queryKey: QueryKey, apply: (current: Data, input: Input) => Data) {
  return {
    onMutate: async (input: Input): Promise<Snapshot<Data>> => {
      await queryClient.cancelQueries({ queryKey });
      const previous = queryClient.getQueryData<Data>(queryKey);
      if (previous) queryClient.setQueryData(queryKey, apply(previous, input));
      return { previous };
    },
    rollback: (context: Snapshot<Data> | undefined) => {
      if (context?.previous) queryClient.setQueryData(queryKey, context.previous);
    },
  };
}

/** `optimisticCache` for a cached list that drops the rows `isGone` names. */
export function optimisticListRemove<Row, Input>(queryClient: QueryClient, queryKey: QueryKey, isGone: (row: Row, input: Input) => boolean) {
  return optimisticCache<readonly Row[], Input>(queryClient, queryKey, (rows, input) => rows.filter((row) => !isGone(row, input)));
}
