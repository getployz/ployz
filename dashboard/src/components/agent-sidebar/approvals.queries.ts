import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { Option, Schema } from "effect";
import { APPROVAL_STATUSES, type ApprovalDecision } from "#/modules/approvals/approvals";
import { getApprovalServerFn, listPendingApprovalsServerFn } from "#/modules/approvals/approvals.functions";

export type ApprovalView = Awaited<ReturnType<typeof getApprovalServerFn>>;

/** How often the sidebar rereads approvals: a CLI or another tab can ask, or answer, at any moment. */
const APPROVAL_POLL_MS = 2_000;

const approvalKeys = (organizationSlug: string) => ["approvals", organizationSlug] as const;

/** Every approval still waiting on a human in the Organization. Drives the collapsed badge and the waiting list. */
export function pendingApprovalsOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...approvalKeys(organizationSlug), "pending"] as const,
    queryFn: () => listPendingApprovalsServerFn({ data: { organizationSlug } }),
    staleTime: 0,
    refetchInterval: APPROVAL_POLL_MS,
  });
}

/** One approval as Cloud holds it now; polled while it waits, since the answer may come from anywhere. */
export function approvalOptions(organizationSlug: string, id: string) {
  return queryOptions({
    queryKey: [...approvalKeys(organizationSlug), id] as const,
    queryFn: () => getApprovalServerFn({ data: { organizationSlug, id } }),
    staleTime: 0,
    refetchInterval: (query) => query.state.data?.status === "pending" ? APPROVAL_POLL_MS : false,
  });
}

const Decided = Schema.Struct({
  approval: Schema.Struct({
    status: Schema.Literals(APPROVAL_STATUSES),
    reason: Schema.NullOr(Schema.String),
    decided_at: Schema.NullOr(Schema.String),
  }),
});
export type Decided = typeof Decided.Type["approval"];
const Refused = Schema.Struct({ error: Schema.Struct({ message: Schema.String }) });

/**
 * Approve exactly the digest shown, or deny with a reason, as the CLI does. Answers the refusal's message when Cloud
 * says no, such as a plan that changed since the card drew.
 */
export async function decideApproval(id: string, decision: ApprovalDecision): Promise<{ ok: true; decided: Decided } | { ok: false; message: string }> {
  const response = await fetch(`/api/cli/approvals/${encodeURIComponent(id)}`, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(decision),
  });
  const body: unknown = await response.json().catch(() => null);
  const decided = Schema.decodeUnknownOption(Decided)(body);
  if (response.ok && Option.isSome(decided)) return { ok: true, decided: decided.value.approval };
  return {
    ok: false,
    message: Option.match(Schema.decodeUnknownOption(Refused)(body), {
      onNone: () => "Cloud didn't take that answer. Try again.",
      onSome: (refused) => refused.error.message,
    }),
  };
}

/** Show a decision at once, then reread every approval: the badge and the waiting list drop it. */
export function landDecision(queryClient: QueryClient, organizationSlug: string, id: string, decided: Decided) {
  queryClient.setQueryData(approvalOptions(organizationSlug, id).queryKey, (row) => row && { ...row, ...decided });
  return refetchApprovals(queryClient, organizationSlug);
}

export function refetchApprovals(queryClient: QueryClient, organizationSlug: string) {
  return queryClient.invalidateQueries({ queryKey: approvalKeys(organizationSlug) });
}
