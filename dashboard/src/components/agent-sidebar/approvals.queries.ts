/** @data-source remote: approvals waiting on a human, and each one a chat names, polled while pending (see `decidable.queries`) */
import type { ApprovalDecision } from "#/modules/approvals/approvals";
import { decidableQueries } from "./decidable.queries";
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

const decidable = decidableQueries({
  root: "approvals",
  list: (organizationSlug: string) => listPendingApprovalsServerFn({ data: { organizationSlug } }),
  get: (organizationSlug: string, id: string) => getApprovalServerFn({ data: { organizationSlug, id } }),
  decide: (organizationSlug: string, id: string, decision: ApprovalDecision) => decideApprovalServerFn({ data: { organizationSlug, id, decision } }),
  unwrap: (answer) => answer.ok ? { ok: true, decided: answer.approval } : answer,
  moving: (approval) => approval?.status === "pending",
});

/** Every approval still waiting on a human in the Organization. Drives the collapsed badge and the waiting list. */
export const pendingApprovalsOptions = decidable.pendingOptions;

/** One approval as Cloud holds it now; polled while it waits, since the answer may come from anywhere. */
export const approvalOptions = decidable.rowOptions;

/** Approve exactly the digest shown, or deny with a reason, as the CLI does. */
export const decideApproval = decidable.decide;

export const landDecision = decidable.land;

export const refetchApprovals = decidable.refetch;
