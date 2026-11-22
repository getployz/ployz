/** @data-source remote: proposals waiting on a human, and each one a card shows, polled while waiting or running (see `decidable.queries`) */
import { decidableQueries } from "./decidable.queries";
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

const decidable = decidableQueries({
  root: "proposals",
  list: (organizationSlug: string) => listPendingProposalsServerFn({ data: { organizationSlug } }),
  get: (organizationSlug: string, id: string) => getProposalServerFn({ data: { organizationSlug, id } }),
  decide: (organizationSlug: string, id: string, decision: Parameters<typeof decideProposalServerFn>[0]["data"]["decision"]) =>
    decideProposalServerFn({ data: { organizationSlug, id, decision } }),
  unwrap: (answer) => answer.ok ? { ok: true, decided: answer.proposal } : answer,
  moving,
});

/** Every proposal waiting on a person in the Organization. Drives the collapsed badge and the waiting list. */
export const pendingProposalsOptions = decidable.pendingOptions;

/** One proposal as Cloud holds it now; polled while it waits or runs, since the answer and each step's end come from anywhere. */
export const proposalOptions = decidable.rowOptions;

/** Approve every step, or deny with a reason. */
export const decideProposal = decidable.decide;

export const landProposal = decidable.land;

export const refetchProposals = decidable.refetch;
