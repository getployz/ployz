/** @data-source remote: the Organization's tokens the member can reach, prefetched by Organization Settings, fresh for 30s and refetched after this tab mints or revokes one; a token made elsewhere shows on the next refetch */
import { queryOptions, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { optimisticListRemove } from "#/lib/optimistic";
import { listTokensServerFn, revokeTokenServerFn } from "#/modules/identity/organization-token.functions";

type Tokens = Awaited<ReturnType<typeof listTokensServerFn>>;

const tokensKey = (organizationSlug: string) => ["organization-tokens", organizationSlug] as const;

export function organizationTokensQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: tokensKey(organizationSlug),
    queryFn: () => listTokensServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}

/** Revokes a token: its row goes at once, and comes back with a toast if the save fails. */
export function useRevokeOrganizationToken(organizationSlug: string) {
  const queryClient = useQueryClient();
  const { onMutate, rollback } = optimisticListRemove<Tokens[number], string>(queryClient, tokensKey(organizationSlug), (token, id) => token.id === id);
  return useMutation({
    mutationFn: (id: string) => revokeTokenServerFn({ data: { organizationSlug, id } }),
    onMutate,
    onError: (cause, _id, context) => {
      rollback(context);
      toast.error(toErrorMessage(cause, "Couldn't revoke the token."));
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: tokensKey(organizationSlug) }),
  });
}

/** Refetches the tokens once this tab has made one. */
export function useRefreshOrganizationTokens(organizationSlug: string) {
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: tokensKey(organizationSlug) });
}
