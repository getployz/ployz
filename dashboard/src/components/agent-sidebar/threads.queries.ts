/** @data-source remote: the member's latest 30 chats, newest first, titled on the server; refetched when a turn in the open chat ends, the only writer of a member's own chats */
import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { listAgentThreadsServerFn } from "#/modules/agent/agent.functions";

export type ThreadSummary = Awaited<ReturnType<typeof listAgentThreadsServerFn>>[number];

const threadKeys = (organizationSlug: string) => ["agent-threads", organizationSlug] as const;

/** The member's latest chats, titled. Only this member's own turns change it, and each finished turn refetches it. */
export function agentThreadsOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: threadKeys(organizationSlug),
    queryFn: () => listAgentThreadsServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}

export function refetchAgentThreads(queryClient: QueryClient, organizationSlug: string) {
  return queryClient.invalidateQueries({ queryKey: threadKeys(organizationSlug) });
}
