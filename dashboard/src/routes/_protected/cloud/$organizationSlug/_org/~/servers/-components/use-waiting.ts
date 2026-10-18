import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { PROBE_WINDOW_MS } from "#/modules/machines/waiting/checkin-wire";
import type { FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";
import { createFleetTokenServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import { cancelServerEnrollmentServerFn, openProbeSelectionServerFn, rejectWaitingServersServerFn } from "#/modules/machines/waiting/waiting.functions";
import { waitingServerDetailQueryOptions, fleetTokenKeys, serverEnrollmentsQueryOptions, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";

export type MintInput = { name: string | null; expiresIn: FleetTokenLifetime; autoEnroll: { readonly maxServers: number } | null };

/** Mints a fleet token. The secret lives only in the result, so it shows once. */
export function useMintFleetToken(organizationSlug: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: MintInput) => createFleetTokenServerFn({ data: { organizationSlug, ...input } }),
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't create the token.")),
    onSettled: () => queryClient.invalidateQueries({ queryKey: fleetTokenKeys.all }),
  });
}

export function useWaitingServers(organizationSlug: string) {
  return useQuery(waitingServersQueryOptions(organizationSlug));
}

export function useRejectWaiting(organizationSlug: string) {
  const queryClient = useQueryClient();
  const { queryKey } = waitingServersQueryOptions(organizationSlug);
  return useMutation({
    mutationFn: (ids: readonly string[]) => rejectWaitingServersServerFn({ data: { organizationSlug, ids } }),
    onMutate: async (ids) => {
      await queryClient.cancelQueries({ queryKey });
      const previous = queryClient.getQueryData(queryKey);
      const gone = new Set(ids);
      if (previous) queryClient.setQueryData(queryKey, previous.filter((server) => !gone.has(server.id)));
      return { previous };
    },
    onError: (cause, _ids, context) => {
      if (context?.previous) queryClient.setQueryData(queryKey, context.previous);
      toast.error(toErrorMessage(cause, "Couldn't reject."));
    },
    onSuccess: ({ rejected }) => toast.success(`Rejected ${rejected} ${rejected === 1 ? "server" : "servers"}`),
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey }),
      queryClient.invalidateQueries({ queryKey: fleetTokenKeys.all }),
    ]),
  });
}

export const PROBE_SETTLE_MS = 800;
export const PROBE_RETRY_MS = PROBE_WINDOW_MS + 1_000;

/**
 * Probes a selection once it stops changing. Cloud won't reopen over a host whose window is still running,
 * so a selection it turns away is asked again once that window has closed, if it is still the selection.
 */
export function useSettledProbe(organizationSlug: string, ids: readonly string[]) {
  const selection = [...ids].sort().join(",");
  useEffect(() => {
    if (selection === "") return;
    let current = true;
    let timer = setTimeout(async function ask(retry = true) {
      const opened = await openProbeSelectionServerFn({ data: { organizationSlug, ids: selection.split(",") } }).catch(() => null);
      if (current && retry && opened?.probing === 0) timer = setTimeout(() => ask(false), PROBE_RETRY_MS);
    }, PROBE_SETTLE_MS);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [organizationSlug, selection]);
}

export function useEnrollmentStatus(organizationSlug: string) {
  return useQuery(serverEnrollmentsQueryOptions(organizationSlug));
}

export function useWaitingServerDetail(organizationSlug: string, id: string | null) {
  return useQuery(waitingServerDetailQueryOptions(organizationSlug, id));
}

export function useCancelEnrollment(organizationSlug: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => cancelServerEnrollmentServerFn({ data: { organizationSlug, id } }),
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't cancel enrollment.")),
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey: serverEnrollmentsQueryOptions(organizationSlug).queryKey }),
      queryClient.invalidateQueries({ queryKey: waitingServersQueryOptions(organizationSlug).queryKey }),
    ]),
  });
}
