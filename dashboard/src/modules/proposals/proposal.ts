import type { ServerRef, ServiceVerb, Signed } from "@ployz/sdk";

export const PROPOSAL_STATUSES = ["draft", "presented", "approved", "denied"] as const;
export type ProposalStatus = (typeof PROPOSAL_STATUSES)[number];

/** How many steps one proposal holds at most. */
export const PROPOSAL_STEP_LIMIT = 10;

/** One staged restart, stop or start, naming the Service as the Servers know it when it was staged. */
export type ProposalStep = {
  readonly verb: ServiceVerb;
  readonly service: string;
  readonly only: ServerRef | null;
  readonly namespace: string;
  readonly runtime: string;
};

/** Why a proposal's steps stopped before they all ran: the step that didn't start, and why. */
export type ProposalHalt = { readonly index: number; readonly reason: string };

/** Where one step stands: its run's state, waiting for an earlier step, or never run because an earlier one failed. */
export type StepProgress = "accepted" | "running" | "done" | "failed" | "unknown" | "waiting" | "not_run";

/** The browser's approval of every Container of each step, by the step's index then the Container. */
export type ProposalApprovals = Readonly<Record<string, Readonly<Record<string, Signed>>>>;

/** A Server from before proposals refuses a step's envelope as unknown bytes, so Cloud sends one only where it is advertised. */
export const PROPOSAL_CAPABILITY = "ployz.restart.proposal.v1";

/**
 * The Server that would refuse a proposal step: it runs a copy of a step's Service and enforces, but doesn't take a
 * proposal. Null when every such Server does.
 */
export function strandedByProposal(
  copies: readonly { readonly machineId: string }[],
  machines: readonly { readonly id: string; readonly name: string; readonly capabilities: readonly string[]; readonly deviceEnforcement?: boolean | null }[],
) {
  const hosts = new Set(copies.map((copy) => copy.machineId));
  return machines.find((machine) => hosts.has(machine.id) && machine.deviceEnforcement === true && !machine.capabilities.includes(PROPOSAL_CAPABILITY))?.name ?? null;
}

const VERBS = { restart: "Restart", stop: "Stop", start: "Start" } as const satisfies Record<ServiceVerb, string>;

/** A step as its card line reads: `Stop web`, or `Stop web on fra-1`. */
export const stepTitle = (step: Pick<ProposalStep, "verb" | "service" | "only">) =>
  `${VERBS[step.verb]} ${step.service}${step.only === null ? "" : ` on ${step.only.name}`}`;

/** What a step takes away while it runs, as the card's Destroys list words it; null for a start. */
export const stepLoss = (step: Pick<ProposalStep, "verb" | "service">) =>
  step.verb === "stop" ? { verb: "Stops service", name: step.service, after: " until started again" }
  : step.verb === "restart" ? { verb: "Restarts service", name: step.service, after: "" }
  : null;

/** Why the approver's browser signs nothing: what Cloud names for a step isn't what the card shows. */
export const MISMATCHED_PROPOSAL = "This doesn't match the proposal. Nothing was approved.";

type ShownStep = Pick<ProposalStep, "verb" | "service" | "only" | "namespace" | "runtime">;
type FetchedStep = Pick<ProposalStep, "verb" | "service" | "namespace" | "runtime"> & {
  readonly copies: readonly { readonly id: string; readonly machineId: string; readonly hook: boolean }[];
};

/**
 * What the browser signs: the card's namespace, and for each step the card's verb and Service as the Servers know it,
 * which Cloud's target must repeat step for step, with the Containers Cloud names for it, each on the Server the card
 * names when it names one. Cloud can't add, drop or swap a step, nor move one to another namespace or Server.
 */
export function shownSteps<F extends FetchedStep>(shown: readonly ShownStep[], fetched: readonly F[]) {
  const namespace = shown[0]?.namespace;
  const matches = namespace !== undefined && shown.length === fetched.length && shown.every((step, index) => {
    const target = fetched[index];
    return target !== undefined && step.verb === target.verb && step.service === target.service
      && step.namespace === namespace && target.namespace === namespace && step.runtime === target.runtime
      && (step.only === null || target.copies.every((copy) => copy.machineId === step.only?.machine));
  });
  if (!matches) throw new Error(MISMATCHED_PROPOSAL);
  return { namespace, steps: shown.map((step, index) => ({ verb: step.verb, runtime: step.runtime, copies: fetched[index]?.copies ?? [] })) };
}
