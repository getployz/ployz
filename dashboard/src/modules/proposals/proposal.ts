import type { ServerRef, ServiceVerb, Signed } from "@ployz/sdk";
import { APPROVAL_RAN_OUT, leftStopped, type ServiceActLeg } from "#/modules/service-act/service-act";

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

/**
 * The name the card shows for a step's Service: the runtime name the approval signs and the Servers act on, with the
 * name the Service has now beside it when a rename left them apart: `web`, or `web-old (web)`.
 */
export const stepName = (step: Pick<ProposalStep, "service" | "runtime">) =>
  step.runtime === step.service ? step.runtime : `${step.runtime} (${step.service})`;

/** A step as its card line reads: `Stop web`, or `Stop web on fra-1`. */
export const stepTitle = (step: Pick<ProposalStep, "verb" | "service" | "runtime" | "only">) =>
  `${VERBS[step.verb]} ${stepName(step)}${step.only === null ? "" : ` on ${step.only.name}`}`;

/** Names as a sentence lists them: `web`, `web and api`, `web, api and db`. */
const listed = (names: readonly string[]) =>
  names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1] ?? ""}`;

/**
 * Why step `index` halted when the approval ran out: what the person has to start now, which is every Service an
 * earlier step stopped and none started again, with this step's own when it is a start or its legs left a copy
 * stopped; or, with nothing stopped, the step left to them.
 */
export function ranOutReason(steps: readonly ProposalStep[], index: number, legs: readonly ServiceActLeg[]) {
  const stopped = new Set<string>();
  for (const step of steps.slice(0, index)) {
    if (step.verb === "stop") stopped.add(stepName(step));
    else stopped.delete(stepName(step));
  }
  const step = steps[index];
  if (step === undefined) return APPROVAL_RAN_OUT;
  if (step.verb === "start" || legs.some(leftStopped)) stopped.add(stepName(step));
  return stopped.size === 0 ? `${APPROVAL_RAN_OUT} ${stepTitle(step)} yourself.` : `${APPROVAL_RAN_OUT} Start ${listed([...stopped])} yourself.`;
}

/** What a step takes away while it runs, as the card's Destroys list words it; null for a start. */
export const stepLoss = (step: Pick<ProposalStep, "verb" | "service" | "runtime">) =>
  step.verb === "stop" ? { verb: "Stops service", name: stepName(step), after: " until started again" }
  : step.verb === "restart" ? { verb: "Restarts service", name: stepName(step), after: "" }
  : null;

/** Why the approver's browser signs nothing: what Cloud names for a step isn't what the card shows. */
export const MISMATCHED_PROPOSAL = "This doesn't match the proposal. Nothing was approved.";

type ShownProposal = {
  readonly project: string;
  readonly environment: string;
  readonly steps: readonly Pick<ProposalStep, "verb" | "service" | "only" | "namespace" | "runtime">[];
};
type FetchedProposal = {
  readonly project: string;
  readonly environmentName: string;
  readonly namespace: string;
  readonly steps: readonly {
    readonly verb: ServiceVerb;
    readonly service: string;
    readonly runtime: string | null;
    readonly copies: readonly { readonly id: string; readonly machineId: string; readonly hook: boolean }[];
  }[];
};

/**
 * What the browser signs: the card's namespace, and for each step the card's verb and the runtime name it shows. The
 * Store, read fresh, must own that namespace under the card's Project and Environment and name that runtime for the
 * step's Service, and Cloud's target must repeat the steps in the card's order, with each step's Containers on the
 * Server the card names when it names one. A row changed under the card, in its names or its place, signs nothing.
 */
export function shownSteps(shown: ShownProposal, fetched: FetchedProposal) {
  const { namespace } = fetched;
  const matches = shown.project === fetched.project && shown.environment === fetched.environmentName
    && shown.steps.length > 0 && shown.steps.length === fetched.steps.length && shown.steps.every((step, index) => {
      const target = fetched.steps[index];
      return target !== undefined && step.verb === target.verb && step.service === target.service
        && step.namespace === namespace && step.runtime === target.runtime
        && (step.only === null || target.copies.every((copy) => copy.machineId === step.only?.machine));
    });
  if (!matches) throw new Error(MISMATCHED_PROPOSAL);
  return { namespace, steps: shown.steps.map((step, index) => ({ verb: step.verb, runtime: step.runtime, copies: fetched.steps[index]?.copies ?? [] })) };
}
