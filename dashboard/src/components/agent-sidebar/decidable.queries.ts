/** @data-source remote: what waits on a human in the Organization, and each one a card shows, polled every 2s while it can still change: any CLI, tab or device can ask or answer, and no change stream carries them */
import { queryOptions, type QueryClient } from "@tanstack/react-query";

/** How often the sidebar rereads what waits on a human: a CLI or another tab can ask, or answer, at any moment. */
const POLL_MS = 2_000;

/**
 * The queries behind one kind of card the sidebar decides: the pending list, one row by id, the decision, and landing it.
 * `root` names its cache keys. `moving` says whether a row can still change, which is when it is polled.
 */
export function decidableQueries<Pending, Got extends object | null, Decided extends NonNullable<Got>, Decision, Answer>(config: {
  root: string;
  list: (organizationSlug: string) => Promise<Pending>;
  get: (organizationSlug: string, id: string) => Promise<Got>;
  decide: (organizationSlug: string, id: string, decision: Decision) => Promise<Answer>;
  /** Cloud's answer to a decision as the row it decided, or its refusal's message. */
  unwrap: (answer: Answer) => { ok: true; decided: Decided } | { ok: false; message: string };
  moving: (row: Got | undefined) => boolean;
}) {
  const keys = (organizationSlug: string) => [config.root, organizationSlug] as const;

  const pendingOptions = (organizationSlug: string) => queryOptions({
    queryKey: [...keys(organizationSlug), "pending"] as const,
    queryFn: () => config.list(organizationSlug),
    staleTime: 0,
    refetchInterval: POLL_MS,
  });

  const rowOptions = (organizationSlug: string, id: string) => queryOptions({
    queryKey: [...keys(organizationSlug), id] as const,
    queryFn: () => config.get(organizationSlug, id),
    staleTime: 0,
    refetchInterval: (query) => config.moving(query.state.data) ? POLL_MS : false,
  });

  /** Answers Cloud's refusal as its message, such as a plan that changed since the card drew. */
  const decide = async (organizationSlug: string, id: string, decision: Decision): Promise<{ ok: true; decided: Decided } | { ok: false; message: string }> => {
    const answer = await config.decide(organizationSlug, id, decision).then(config.unwrap, () => null);
    return answer ?? { ok: false, message: "Cloud didn't take that answer. Try again." };
  };

  const refetch = (queryClient: QueryClient, organizationSlug: string) =>
    queryClient.invalidateQueries({ queryKey: keys(organizationSlug) });

  /** Show a decision at once, then reread everything: the badge and the waiting list drop it. */
  const land = (queryClient: QueryClient, organizationSlug: string, id: string, decided: Decided) => {
    queryClient.setQueryData(rowOptions(organizationSlug, id).queryKey, (row) => row && { ...row, ...decided });
    return refetch(queryClient, organizationSlug);
  };

  return { pendingOptions, rowOptions, decide, refetch, land };
}
