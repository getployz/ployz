import { queryOptions, type QueryClient } from "@tanstack/react-query";
import type { Signed } from "@ployz/sdk";
import { asRecord, asString } from "#/lib/json";
import { restartApprovals, type RestartTarget } from "#/modules/approval-session/approval-prompt";
import type { ApprovalDecision } from "#/modules/approvals/approvals";
import {
  approvalRestartTargetServerFn,
  decideApprovalServerFn,
  getApprovalServerFn,
  listPendingApprovalsServerFn,
} from "#/modules/approvals/approvals.functions";

export type ApprovalView = Awaited<ReturnType<typeof getApprovalServerFn>>;

/** Whether `approval` waits on this person, rather than on an owner or admin they asked into a Protected Environment. */
export const waitsOnYou = (approval: Pick<ApprovalView, "protected" | "yours">) => !(approval.protected && approval.yours);

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

export type Decided = ApprovalView;

/**
 * Approve exactly the digest shown, or deny with a reason, as the CLI does. Answers the refusal's message when Cloud
 * says no, such as a plan that changed since the card drew.
 */
export async function decideApproval(
  organizationSlug: string,
  id: string,
  decision: ApprovalDecision,
  approvals?: Readonly<Record<string, Signed>>,
): Promise<{ ok: true; decided: Decided } | { ok: false; message: string }> {
  const answer = await decideApprovalServerFn({ data: { organizationSlug, id, decision, approvals } }).catch(() => null);
  if (answer === null) return { ok: false, message: "Cloud didn't take that answer. Try again." };
  return answer.ok ? { ok: true, decided: answer.approval } : answer;
}

/** Whether approving `approval` runs a restart, which Servers that enforce take only with the approver's passkey. */
export const runsRestart = (approval: Pick<ApprovalView, "review">) =>
  "operation" in approval.review && approval.review.operation.verb === "restart";

/** The Environment and Service a restart card shows, from the review its digest covers. */
function shownRestart(approval: Pick<ApprovalView, "review">) {
  if (!("operation" in approval.review) || approval.review.operation.verb !== "restart") return null;
  const preview = asRecord(approval.review.operation.preview);
  const environment = asString(preview?.["environment"]);
  const service = asString(preview?.["service"]);
  return environment === null || service === null ? null : { environment, service };
}

/** Why the approver's browser makes no approvals: the restart Cloud names isn't the one on the card. */
export const MISMATCHED_RESTART = "This restart doesn't match the request. Nothing was approved.";

type FetchedTarget = NonNullable<Awaited<ReturnType<typeof approvalRestartTargetServerFn>>>;

/**
 * What the browser approves for the restart `approval` shows: Cloud's target, only when its Environment and Service are
 * the ones on the card, so Cloud can't swap in another Service's containers.
 */
export function shownTarget(approval: Pick<ApprovalView, "review">, target: FetchedTarget): RestartTarget {
  const shown = shownRestart(approval);
  if (shown === null || shown.environment !== target.environmentId || shown.service !== target.service) {
    throw new Error(MISMATCHED_RESTART);
  }
  const { environmentId, environmentName, namespace, runtime, copies, machines, environmentMachines } = target;
  return { environment: { environmentId, environmentName, namespace }, runtime, copies, machines, environmentMachines };
}

/**
 * The approvals a restart ask needs once approved, made in this browser after one passkey prompt. Undefined when it
 * needs none; null when the person cancels. Throws, approving nothing, when Cloud's target isn't the card's restart.
 */
export async function approverApprovals(organizationSlug: string, approval: Pick<ApprovalView, "id" | "review">) {
  const target = await approvalRestartTargetServerFn({ data: { organizationSlug, id: approval.id } });
  return target === null ? undefined : restartApprovals(shownTarget(approval, target));
}

/** Show a decision at once, then reread every approval: the badge and the waiting list drop it. */
export function landDecision(queryClient: QueryClient, organizationSlug: string, id: string, decided: Decided) {
  queryClient.setQueryData(approvalOptions(organizationSlug, id).queryKey, (row) => row && { ...row, ...decided });
  return refetchApprovals(queryClient, organizationSlug);
}

export function refetchApprovals(queryClient: QueryClient, organizationSlug: string) {
  return queryClient.invalidateQueries({ queryKey: approvalKeys(organizationSlug) });
}
