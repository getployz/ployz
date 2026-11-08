import { queryOptions, type QueryClient } from "@tanstack/react-query";
import type { Signed } from "@ployz/sdk";
import { actApprovals, type ActTarget } from "#/modules/approval-session/approval-prompt";
import { type ApprovalDecision, approvalSubject, shownServiceAct } from "#/modules/approvals/approvals";
import {
  approvalActTargetServerFn,
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

/** Whether approving `approval` runs a restart or stop, which Servers that enforce take only with the approver's passkey. */
export const runsServiceAct = (approval: Pick<ApprovalView, "review">) =>
  "operation" in approval.review && (approval.review.operation.verb === "restart" || approval.review.operation.verb === "stop");

/** Why the approver's browser makes no approvals: what Cloud names isn't what the card shows. */
export const MISMATCHED_ACT = "This doesn't match the request. Nothing was approved.";

type FetchedTarget = NonNullable<Awaited<ReturnType<typeof approvalActTargetServerFn>>>;

/**
 * What the browser approves for the restart or stop `approval` shows. Cloud's target must name the card's verb,
 * Environment, Service, Namespace and runtime name, so Cloud can't swap in another act or Service, and the card's
 * title and Destroys line must name that same Service in the Store's place for that Environment, so no name the person
 * reads differs from what gets signed. The approvals carry the card's values.
 */
export function shownTarget(approval: Pick<ApprovalView, "command" | "review">, target: FetchedTarget) {
  const shown = shownServiceAct(approval.review);
  const matches = shown !== null && shown.verb === target.verb && shown.environment === target.environmentId && shown.service === target.service
    && shown.namespace === target.namespace && shown.runtime === target.runtime
    && approvalSubject(approval.command, approval.review).name === shownName(shown, target)
    && approval.review.effects.every((effect) => effect.name === shown.service);
  if (!matches) throw new Error(MISMATCHED_ACT);
  // What gets signed comes from the card. Containers and Servers come from Cloud; each Server checks them against its own.
  const { environmentName, copies, machines, environmentMachines } = target;
  return {
    verb: shown.verb,
    target: {
      environment: { environmentId: shown.environment, environmentName, namespace: shown.namespace },
      runtime: shown.runtime,
      copies,
      machines,
      environmentMachines,
    } satisfies ActTarget,
  };
}

/** The card's title for the act, as Cloud words it when it asks: `web in shop/production`, or `… on fra-1`. Null for a Server Cloud doesn't list. */
const shownName = (shown: NonNullable<ReturnType<typeof shownServiceAct>>, target: FetchedTarget) => {
  const place = `${shown.service} in ${target.project}/${target.environmentName}`;
  if (shown.only === null) return place;
  const server = target.machines.find((machine) => machine.id === shown.only?.machine);
  return server === undefined ? null : `${place} on ${server.name}`;
};

export async function approverApprovals(organizationSlug: string, approval: Pick<ApprovalView, "id" | "command" | "review">) {
  const fetched = await approvalActTargetServerFn({ data: { organizationSlug, id: approval.id } });
  if (fetched === null) return undefined;
  const { verb, target } = shownTarget(approval, fetched);
  return actApprovals(verb, target);
}

/** Show a decision at once, then reread every approval: the badge and the waiting list drop it. */
export function landDecision(queryClient: QueryClient, organizationSlug: string, id: string, decided: Decided) {
  queryClient.setQueryData(approvalOptions(organizationSlug, id).queryKey, (row) => row && { ...row, ...decided });
  return refetchApprovals(queryClient, organizationSlug);
}

export function refetchApprovals(queryClient: QueryClient, organizationSlug: string) {
  return queryClient.invalidateQueries({ queryKey: approvalKeys(organizationSlug) });
}
