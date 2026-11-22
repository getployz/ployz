import type { ServerRef, ServiceVerb } from "@ployz/sdk";
import { destroyedLine } from "#/modules/approvals/approvals";

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

/** What a step takes away while it runs, as the card's Destroys list words it; null for a start. */
export const stepLoss = (step: Pick<ProposalStep, "verb" | "service" | "runtime">) =>
  step.verb === "start" ? null
  : destroyedLine({ kind: step.verb === "stop" ? "stops_service" : "restarts_service", name: stepName(step), node: "", path: "" });
