/** @data-source remote: approvals waiting on a human, and each one a chat names, polled every 2s while pending: any CLI, tab or device can ask or answer, and no change stream carries them */
import { queryOptions } from "@tanstack/react-query";
import type { ApprovalDecision } from "#/modules/approvals/approvals";
import { decidable, DECIDABLE_POLL_MS } from "#/modules/proposals/decidable";
import {
  decideApprovalServerFn,
  getApprovalServerFn,
  listPendingApprovalsServerFn,
} from "#/modules/approvals/approvals.functions";

export type ApprovalView = Awaited<ReturnType<typeof getApprovalServerFn>>;

/** Whether `approval` waits on this person, rather than on an owner or admin they asked into a Protected Environment. */
export const waitsOnYou = (approval: Pick<ApprovalView, "protected" | "yours">) => !(approval.protected && approval.yours);

/** Who must answer before a chat that asked for `approvals` can go on: this person, unless none of them is theirs to answer. */
export const answeredBy = (approvals: readonly Pick<ApprovalView, "protected" | "yours">[]): "you" | "others" =>
  approvals.length > 0 && !approvals.some(waitsOnYou) ? "others" : "you";

const approvals = decidable({
  root: "approvals",
  decide: (organizationSlug: string, id: string, decision: ApprovalDecision) => decideApprovalServerFn({ data: { organizationSlug, id, decision } }),
  unwrap: (answer) => answer.ok ? { ok: true, decided: answer.approval } : answer,
});

/** Every approval still waiting on a human in the Organization. Drives the collapsed badge and the waiting list. */
export function pendingApprovalsOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: approvals.pendingKey(organizationSlug),
    queryFn: () => listPendingApprovalsServerFn({ data: { organizationSlug } }),
    staleTime: 0,
    refetchInterval: DECIDABLE_POLL_MS,
  });
}

/** One approval as Cloud holds it now; polled while it waits, since the answer may come from anywhere. */
export function approvalOptions(organizationSlug: string, id: string) {
  return queryOptions({
    queryKey: approvals.rowKey(organizationSlug, id),
    queryFn: () => getApprovalServerFn({ data: { organizationSlug, id } }),
    staleTime: 0,
    refetchInterval: (query) => query.state.data?.status === "pending" ? DECIDABLE_POLL_MS : false,
  });
}

export const decideApproval = approvals.decide;

export const landDecision = approvals.land;

export const refetchApprovals = approvals.refetch;
