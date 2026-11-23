import type { ServerRef, ServiceVerb } from "@ployz/sdk";
import { destroyedLine } from "#/modules/approvals/approvals";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";

export const PROPOSAL_STATUSES = ["draft", "presented", "approved", "denied"] as const;
export type ProposalStatus = (typeof PROPOSAL_STATUSES)[number];

/** How many steps one proposal holds at most. */
export const PROPOSAL_STEP_LIMIT = 10;

/** One staged restart, stop or start, naming the Service as the Servers know it when it was staged. */
export type ServiceStep = {
  readonly verb: ServiceVerb;
  readonly service: string;
  readonly only: ServerRef | null;
  readonly namespace: string;
  readonly runtime: string;
};

/**
 * A Deploy a caller that can't sign asked for, held whole as the Store will admit it: its Environment, Services, flags,
 * upload and the review version it is bound to, and once a person approved it, the versions their passkey signed.
 */
export type DeployStep = { readonly verb: "deploy"; readonly deploy: { readonly command: AdmitDeploy } };

export type ProposalStep = ServiceStep | DeployStep;

export const isDeploy = (step: ProposalStep): step is DeployStep => step.verb === "deploy";

/** Why a Deploy's proposal was denied when its Environment moved after it was asked for. */
export const DEPLOY_MOVED = "This deploy changed since it was asked for. Ask again.";

/** Why a proposal's steps stopped before they all ran: the step that didn't start, and why. */
export type ProposalHalt = { readonly index: number; readonly reason: string };

/** Where one step stands: its run's state, waiting for an earlier step, or never run because an earlier one failed. */
export type StepProgress = "accepted" | "running" | "done" | "failed" | "unknown" | "waiting" | "not_run";

const VERBS = { restart: "Restart", stop: "Stop", start: "Start" } as const satisfies Record<ServiceVerb, string>;

/**
 * The name the card shows for a step's Service: the runtime name the approval signs and the Servers act on, with the
 * name the Service has now beside it when a rename left them apart: `web`, or `web-old (web)`.
 */
export const stepName = (step: Pick<ServiceStep, "service" | "runtime">) =>
  step.runtime === step.service ? step.runtime : `${step.runtime} (${step.service})`;

/** What a step's Deploy ships: the Services it names, or every one. */
const shipped = ({ deploy }: DeployStep) => (deploy.command.services ?? []).join(", ") || "all services";

/** A step as its card line reads: `Stop web`, `Stop web on fra-1`, or `Deploy web, api`. */
export const stepTitle = (step: ProposalStep) =>
  isDeploy(step) ? `Deploy ${shipped(step)}` : `${VERBS[step.verb]} ${stepName(step)}${step.only === null ? "" : ` on ${step.only.name}`}`;

/** The Service a step acts on, for the lines that list it: a Deploy names what it ships. */
export const stepService = (step: ProposalStep) => (isDeploy(step) ? shipped(step) : step.service);

/** What a step takes away while it runs, as the card's Destroys list words it; null for a start, and for a Deploy, whose page lists it. */
export const stepLoss = (step: ProposalStep) =>
  isDeploy(step) || step.verb === "start" ? null
  : destroyedLine({ kind: step.verb === "stop" ? "stops_service" : "restarts_service", name: stepName(step), node: "", path: "" });
