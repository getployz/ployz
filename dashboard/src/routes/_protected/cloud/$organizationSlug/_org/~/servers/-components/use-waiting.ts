import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { openProbeSelectionServerFn, rejectWaitingServersServerFn } from "#/modules/machines/waiting/waiting.functions";
import { fleetTokenKeys, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";

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

export function useOpenProbe(organizationSlug: string) {
  return useMutation({
    mutationFn: (ids: readonly string[]) => openProbeSelectionServerFn({ data: { organizationSlug, ids } }),
  });
}
