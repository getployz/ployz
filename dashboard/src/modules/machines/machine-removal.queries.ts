/** @data-source remote: one Machine removal attempt, started by the user's confirmed Remove or Forget; polled every second until it settles, because the Servers finish it and no row change reaches the tab in time */
import { queryOptions, QueryObserver, type QueryClient } from "@tanstack/react-query";
import { getMachineRemoveAttemptServerFn } from "#/modules/machines/machine-removal.functions";
import type { MachineRemoveResult } from "#/modules/machines/machine-removal-states";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";

const POLL_MS = 1_000;

export function machineRemoveAttemptQueryOptions(organizationSlug: string, attemptId: string) {
  return queryOptions({
    queryKey: ["machine-remove-attempt", organizationSlug, attemptId] as const,
    queryFn: () => getMachineRemoveAttemptServerFn({ data: { organizationSlug, attemptId } }),
    staleTime: 0,
    gcTime: 0,
    retry: false,
    refetchInterval: (query) => {
      const state = query.state.data?.state;
      return state === "pending" || state === "running" ? POLL_MS : false;
    },
    refetchIntervalInBackground: true,
  });
}

/**
 * Waits for the removal to settle. `stopped` is checked after every answer: closing the dialog or leaving the page
 * ends the wait within a second.
 */
export function waitForMachineRemoveAttempt(
  queryClient: QueryClient,
  organizationSlug: string,
  attemptId: string,
  stopped: () => boolean,
): Promise<MachineRemoveResult | "aborted" | { missing: DataLossIdentity[] }> {
  if (stopped()) return Promise.resolve("aborted");
  return new Promise((resolve, reject) => {
    const observer = new QueryObserver(queryClient, machineRemoveAttemptQueryOptions(organizationSlug, attemptId));
    const settle = (end: () => void) => {
      unsubscribe();
      end();
    };
    const unsubscribe = observer.subscribe((result) => {
      if (result.isError) return settle(() => reject(result.error));
      const attempt = result.data;
      if (attempt === undefined) return;
      if (stopped()) return settle(() => resolve("aborted"));
      switch (attempt.state) {
        case "pending":
        case "running":
          return;
        case "succeeded":
          return settle(() => resolve(attempt.result));
        case "missing_identities":
          return settle(() => resolve({ missing: attempt.missingIdentities }));
        case "failed":
        case "cancelled":
          return settle(() => reject(new Error(attempt.failureMessage)));
        default: {
          const _exhaustive: never = attempt;
          return settle(() => reject(new Error(`Unhandled machine remove attempt: ${_exhaustive}`)));
        }
      }
    });
  });
}
