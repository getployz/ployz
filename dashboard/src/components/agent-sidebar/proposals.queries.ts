import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { proposalApprovals } from "#/modules/approval-session/approval-prompt";
import { MISMATCHED_PROPOSAL, shownSteps } from "#/modules/proposals/proposal";
import {
  decideProposalServerFn,
  getProposalServerFn,
  listPendingProposalsServerFn,
  proposalTargetServerFn,
} from "#/modules/proposals/proposals.functions";

export type ProposalView = NonNullable<Awaited<ReturnType<typeof getProposalServerFn>>>;

/** How often the sidebar rereads proposals: an agent can present, or another tab decide, at any moment. */
const PROPOSAL_POLL_MS = 2_000;

const proposalKeys = (organizationSlug: string) => ["proposals", organizationSlug] as const;

/** Every proposal waiting on a person in the Organization. Drives the collapsed badge and the waiting list. */
export function pendingProposalsOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...proposalKeys(organizationSlug), "pending"] as const,
    queryFn: () => listPendingProposalsServerFn({ data: { organizationSlug } }),
    staleTime: 0,
    refetchInterval: PROPOSAL_POLL_MS,
  });
}

/** A proposal that can still change: waiting on a person, or approved with a step yet to end. */
const moving = (proposal: ProposalView | null | undefined) =>
  proposal?.status === "presented"
  || (proposal?.status === "approved" && proposal.progress.some((step) => step.state === "waiting" || step.state === "accepted" || step.state === "running"));

/** One proposal as Cloud holds it now; polled while it waits or runs, since the answer and each step's end come from anywhere. */
export function proposalOptions(organizationSlug: string, id: string) {
  return queryOptions({
    queryKey: [...proposalKeys(organizationSlug), id] as const,
    queryFn: () => getProposalServerFn({ data: { organizationSlug, id } }),
    staleTime: 0,
    refetchInterval: (query) => moving(query.state.data) ? PROPOSAL_POLL_MS : false,
  });
}

/**
 * The bundle over every step the card shows, made in this browser after one passkey prompt. Null when the person
 * cancels. Throws, signing nothing, when Cloud's steps aren't the card's or Cloud couldn't read the Servers.
 */
export async function approverBundle(organizationSlug: string, proposal: Pick<ProposalView, "id" | "steps" | "environment" | "environment_id">) {
  const fetched = await proposalTargetServerFn({ data: { organizationSlug, id: proposal.id } });
  if (fetched === null || fetched.environmentId !== proposal.environment_id) throw new Error("Cloud couldn't check the servers. Try again.");
  const { namespace, steps } = shownSteps(proposal.steps, fetched.steps);
  if (fetched.namespace !== namespace) throw new Error(MISMATCHED_PROPOSAL);
  // Every signed value is the card's; the Containers and Servers come from Cloud and each Server checks them against its own.
  const environment = { environmentId: proposal.environment_id, environmentName: proposal.environment, namespace };
  return proposalApprovals({ environment, steps, machines: fetched.machines, environmentMachines: fetched.environmentMachines });
}

/** Approve every step with the bundle this browser made, or deny with a reason. Answers Cloud's refusal as its message. */
export async function decideProposal(
  organizationSlug: string,
  id: string,
  decision: Parameters<typeof decideProposalServerFn>[0]["data"]["decision"],
): Promise<{ ok: true; decided: ProposalView } | { ok: false; message: string }> {
  const answer = await decideProposalServerFn({ data: { organizationSlug, id, decision } }).catch(() => null);
  if (answer === null) return { ok: false, message: "Cloud didn't take that answer. Try again." };
  return answer.ok ? { ok: true, decided: answer.proposal } : answer;
}

/** Show a decision at once, then reread every proposal: the badge and the waiting list drop it. */
export function landProposal(queryClient: QueryClient, organizationSlug: string, id: string, decided: ProposalView) {
  queryClient.setQueryData(proposalOptions(organizationSlug, id).queryKey, (row) => row && { ...row, ...decided });
  return refetchProposals(queryClient, organizationSlug);
}

export function refetchProposals(queryClient: QueryClient, organizationSlug: string) {
  return queryClient.invalidateQueries({ queryKey: proposalKeys(organizationSlug) });
}
