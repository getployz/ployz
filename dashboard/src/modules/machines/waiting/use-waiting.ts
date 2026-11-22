import { refusalFor } from "#/modules/organization/permissions";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { toast } from "sonner";
import { useExclusiveMutation } from "#/hooks/use-exclusive-mutation";
import { toErrorMessage } from "#/lib/error-message";
import { optimisticListRemove } from "#/lib/optimistic";
import { PROBE_WINDOW_MS } from "#/modules/machines/waiting/checkin-wire";
import type { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import type { FleetTokenLifetime, FleetTokenView } from "#/modules/machines/waiting/fleet-token";
import { createFleetTokenServerFn, revokeFleetTokenServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import { cancelServerEnrollmentServerFn, openProbeSelectionServerFn, rejectWaitingServersServerFn, startServerEnrollmentServerFn } from "#/modules/machines/waiting/waiting.functions";
import { waitingServerDetailQueryOptions, fleetTokenKeys, fleetTokensQueryOptions, serverEnrollmentsQueryOptions, waitingKeys, waitingServersQueryOptions, type WaitingServer } from "#/modules/machines/waiting/waiting.queries";
import { useYouMay } from "#/modules/organization/members.queries";

export type MintInput = { name: string | null; expiresIn: FleetTokenLifetime; autoEnroll: { readonly maxServers: number } | null; foundsClosed?: boolean };

export const ADD_SERVERS_REFUSED = refusalFor("servers:add", "add servers");
export const ENROLL_REFUSED = refusalFor("servers:add", "enroll or reject servers");

/** Mints a fleet token. The secret lives only in the result, so it shows once. */
export function useMintFleetToken(organizationSlug: string) {
  const queryClient = useQueryClient();
  return useExclusiveMutation({
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
  const { onMutate, rollback } = optimisticListRemove<WaitingServer, readonly string[]>(queryClient, queryKey, (server, ids) => ids.includes(server.id));
  return useMutation({
    mutationFn: (ids: readonly string[]) => rejectWaitingServersServerFn({ data: { organizationSlug, ids } }),
    onMutate,
    onError: (cause, _ids, context) => {
      rollback(context);
      toast.error(toErrorMessage(cause, "Couldn't reject."));
    },
    onSuccess: ({ rejected }) => toast.success(`Rejected ${rejected} ${rejected === 1 ? "server" : "servers"}`),
    // Rejecting a server can change the count a Fleet Token shows.
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey }),
      queryClient.invalidateQueries({ queryKey: fleetTokenKeys.all }),
    ]),
  });
}

export function useRevokeFleetToken(organizationSlug: string) {
  const queryClient = useQueryClient();
  const { queryKey } = fleetTokensQueryOptions(organizationSlug);
  const { onMutate, rollback } = optimisticListRemove<FleetTokenView, FleetTokenView>(queryClient, queryKey, (row, token) => row.id === token.id);
  return useMutation({
    mutationFn: (token: FleetTokenView) => revokeFleetTokenServerFn({ data: { organizationSlug, id: token.id } }),
    onMutate,
    onError: (cause, _token, context) => {
      rollback(context);
      toast.error(toErrorMessage(cause, "Couldn't revoke."));
    },
    onSuccess: ({ waitingRemoved }, token) =>
      toast.success(`Revoked ${token.name}${waitingRemoved > 0 ? ` · ${waitingRemoved} waiting removed` : ""}`),
    // Revoking removes the Waiting servers that arrived on the token.
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey }),
      queryClient.invalidateQueries({ queryKey: waitingKeys.all }),
    ]),
  });
}

export type EnrollStart = EnrollInput & { regions: Record<string, string> };
type Enrolled = Awaited<ReturnType<typeof startServerEnrollmentServerFn>>;

/**
 * Starts enrolling the chosen Waiting servers. `onEnrolled` runs on success even if the sheet has closed, with the
 * `context` the start was asked with.
 */
export function useStartEnrollment<Context>(organizationSlug: string, onEnrolled: (result: Enrolled, context: Context) => void) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ start }: { start: EnrollStart; context: Context }) => startServerEnrollmentServerFn({ data: { organizationSlug, ...start } }),
    onSuccess: (result, { context }) => onEnrolled(result, context),
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't enroll servers.")),
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey: waitingServersQueryOptions(organizationSlug).queryKey }),
      queryClient.invalidateQueries({ queryKey: serverEnrollmentsQueryOptions(organizationSlug).queryKey }),
      queryClient.invalidateQueries({ queryKey: fleetTokenKeys.all }),
    ]),
  });
}

export const PROBE_SETTLE_MS = 800;
export const PROBE_RETRY_MS = PROBE_WINDOW_MS + 1_000;

/**
 * Probes a selection once it stops changing. Cloud won't reopen over a host whose window is still running,
 * so a selection it turns away is asked again once that window has closed, if it is still the selection.
 * Cloud opens probes only for a role that may add servers, so nothing is asked until your role says so.
 */
export function useSettledProbe(organizationSlug: string, ids: readonly string[]) {
  const selection = [...ids].sort().join(",");
  const may = useYouMay(organizationSlug, "servers:add");
  useEffect(() => {
    if (selection === "" || may !== true) return;
    let current = true;
    let timer = setTimeout(async function ask(retry = true) {
      const opened = await openProbeSelectionServerFn({ data: { organizationSlug, ids: selection.split(",") } }).catch(() => null);
      if (current && retry && opened?.probing === 0) timer = setTimeout(() => ask(false), PROBE_RETRY_MS);
    }, PROBE_SETTLE_MS);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [organizationSlug, selection, may]);
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
