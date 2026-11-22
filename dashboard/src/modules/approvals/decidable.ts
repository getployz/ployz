import type { QueryClient } from "@tanstack/react-query";

/** How often the sidebar rereads what waits on a human: a CLI or another tab can ask, or answer, at any moment. */
export const DECIDABLE_POLL_MS = 2_000;

/**
 * What approvals and proposals share beyond their reads: cache keys under `root`, the decision, and landing it.
 * `unwrap` turns Cloud's answer to a decision into the row it decided, or its refusal's message.
 */
export function decidable<Decided extends object, Decision, Answer>(config: {
  root: string;
  decide: (organizationSlug: string, id: string, decision: Decision) => Promise<Answer>;
  unwrap: (answer: Answer) => { ok: true; decided: Decided } | { ok: false; message: string };
}) {
  const keys = (organizationSlug: string) => [config.root, organizationSlug] as const;
  const pendingKey = (organizationSlug: string) => [...keys(organizationSlug), "pending"] as const;
  const rowKey = (organizationSlug: string, id: string) => [...keys(organizationSlug), id] as const;

  /** Answers Cloud's refusal as its message, such as a plan that changed since the card drew. */
  const decide = async (organizationSlug: string, id: string, decision: Decision): Promise<{ ok: true; decided: Decided } | { ok: false; message: string }> => {
    const answer = await config.decide(organizationSlug, id, decision).then(config.unwrap, () => null);
    return answer ?? { ok: false, message: "Cloud didn't take that answer. Try again." };
  };

  const refetch = (queryClient: QueryClient, organizationSlug: string) =>
    queryClient.invalidateQueries({ queryKey: keys(organizationSlug) });

  /** Show a decision at once, then reread everything: the badge and the waiting list drop it. */
  const land = (queryClient: QueryClient, organizationSlug: string, id: string, decided: Decided) => {
    queryClient.setQueryData<Decided | null>(rowKey(organizationSlug, id), (row) => row && { ...row, ...decided });
    return refetch(queryClient, organizationSlug);
  };

  return { pendingKey, rowKey, decide, refetch, land };
}
