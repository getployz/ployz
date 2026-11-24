/** @data-source remote: proposals waiting on a human, and each one a card shows, polled every 2s while waiting or running: an agent can present, another tab decide, and each step end at any moment */
import { queryOptions } from "@tanstack/react-query";
import { decidable, DECIDABLE_POLL_MS } from "#/modules/proposals/decidable";
import {
  decideProposalServerFn,
  getProposalServerFn,
  listPendingProposalsServerFn,
} from "#/modules/proposals/proposals.functions";

export type ProposalView = NonNullable<Awaited<ReturnType<typeof getProposalServerFn>>>;

/** A proposal that can still change: waiting on a person, or approved with a step yet to end. */
const moving = (proposal: ProposalView | null | undefined) =>
  proposal?.status === "presented"
  || (proposal?.status === "approved" && proposal.progress.some((step) => step.state === "waiting" || step.state === "accepted" || step.state === "running"));

const proposals = decidable({
  root: "proposals",
  decide: (organizationSlug: string, id: string, decision: Parameters<typeof decideProposalServerFn>[0]["data"]["decision"]) =>
    decideProposalServerFn({ data: { organizationSlug, id, decision } }),
  unwrap: (answer) => answer.ok ? { ok: true, decided: answer.proposal } : answer,
});

/** Every proposal waiting on a person in the Organization. Drives the collapsed badge and the waiting list. */
export function pendingProposalsOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: proposals.pendingKey(organizationSlug),
    queryFn: () => listPendingProposalsServerFn({ data: { organizationSlug } }),
    staleTime: 0,
    refetchInterval: DECIDABLE_POLL_MS,
  });
}

/** One proposal as Cloud holds it now; polled while it waits or runs, since the answer and each step's end come from anywhere. */
export function proposalOptions(organizationSlug: string, id: string) {
  return queryOptions({
    queryKey: proposals.rowKey(organizationSlug, id),
    queryFn: () => getProposalServerFn({ data: { organizationSlug, id } }),
    staleTime: 0,
    refetchInterval: (query) => moving(query.state.data) ? DECIDABLE_POLL_MS : false,
  });
}

/** Approve every step, or deny with a reason. */
export const decideProposal = proposals.decide;

export const landProposal = proposals.land;

export const refetchProposals = proposals.refetch;
