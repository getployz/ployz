import { type QueryClient, queryOptions } from "@tanstack/react-query";
import type { PrivateAccessChanged, PrivateAccessWant } from "./private-access";
import { observePrivateAccessServerFn } from "./private-access.functions";

export const privateAccessKeys = { all: ["private-access"] as const };

export function privateAccessQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...privateAccessKeys.all, organizationSlug] as const,
    queryFn: () => observePrivateAccessServerFn({ data: { organizationSlug } }),
    staleTime: 10_000,
    refetchInterval: 15_000,
  });
}

/** The Servers' last answer to a Connect or Turn off in this tab, from Integrations or an Approval card. */
export type PrivateAccessAnswer = { readonly to: PrivateAccessWant; readonly changed: PrivateAccessChanged; readonly at: number };

export function lastAnswerOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["private-access-answer", organizationSlug] as const,
    queryFn: (): PrivateAccessAnswer | null => null,
    staleTime: Infinity,
  });
}

export function landAnswer(queryClient: QueryClient, organizationSlug: string, answer: PrivateAccessAnswer) {
  queryClient.setQueryData(lastAnswerOptions(organizationSlug).queryKey, answer);
  return queryClient.invalidateQueries({ queryKey: privateAccessKeys.all });
}
