import type { ContainerState, Leg, Receipt, RunState, ServerRef, ServiceVerb, Signed } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { type OlderServer, olderServer, olderSays } from "#/modules/runtime/daemon-compat";

export const SERVICE_ACT_STATES = ["accepted", "running", "done", "failed", "unknown"] as const satisfies readonly RunState[];
export type ServiceActState = (typeof SERVICE_ACT_STATES)[number];
export const SERVICE_ACT_VERBS = ["restart", "start", "stop"] as const satisfies readonly ServiceVerb[];
export type ActiveServiceActState = Extract<ServiceActState, "accepted" | "running">;

export type LegStep = "sent" | "done" | "already_there" | "failed" | "unknown";

/** A hook copy is stopped with its Service but never started. */
export type ServiceCopy = { server: string; machine: string; container: string; state: ContainerState; hook: boolean };
export type ServiceActBefore = { copies: ServiceCopy[]; unanswered: ServerRef[] };
export type ServiceActLeg = {
  server: string;
  machine: string;
  container: string;
  hook: boolean;
  stop: LegStep | null;
  start: LegStep | null;
  reason: string | null;
};

export const SERVICE_ACT_PENDING_LIMIT_MS = 15 * 60_000;
export const SERVICE_ACT_RUNNING_LIMIT_MS = 60 * 60_000;

const STOPPED: ReadonlySet<ContainerState> = new Set(["created", "exited", "dead"]);

export const stepsOf = (verb: ServiceVerb) => ({ stop: verb !== "start", start: verb !== "stop" });

export const legsOf = (before: ServiceActBefore, only: ServerRef | null, verb: ServiceVerb): ServiceActLeg[] =>
  before.copies
    .filter((copy) => (only === null || copy.machine === only.machine) && (verb !== "start" || !copy.hook))
    .map(({ server, machine, container, hook }) => ({ server, machine, container, hook, stop: null, start: null, reason: null }));

/** Whether `leg` takes `side`: hooks are stopped, never started. */
export const takes = (leg: ServiceActLeg, side: "stop" | "start") => side === "stop" || !leg.hook;

export const alreadyThere = (verb: ServiceVerb, side: "stop" | "start", state: ContainerState | undefined) =>
  side === "stop" ? state !== undefined && STOPPED.has(state) : verb === "start" && state === "running";

export type ServiceActEnd = { state: Exclude<ServiceActState, ActiveServiceActState>; reason: string | null };

const confirmed = (step: LegStep | null) => step === "done" || step === "already_there";

/** A copy a run stopped, or found stopped, and hasn't confirmed started again: what the person has to start now. */
export const leftStopped = (leg: ServiceActLeg) => confirmed(leg.stop) && takes(leg, "start") && !confirmed(leg.start);

export const APPROVAL_RAN_OUT = "The approval ran out.";

const ApprovalWindow = Schema.fromJsonString(Schema.Struct({ expires_at: Schema.Number }));

/**
 * Whether the approval a leg would carry has run out at `now`: a proposal step's by its bundle, a single act's by its
 * own body, so a Server is never asked to move a copy past what the person was shown. One Cloud can't read goes, and
 * the Server judges it.
 */
export const ranOut = (approval: Signed | undefined, now: number) => {
  if (approval === undefined) return false;
  const window = Schema.decodeUnknownOption(ApprovalWindow)(approval.proposal?.body ?? approval.body);
  return Option.isSome(window) && now > window.value.expires_at;
};

/** A restart rolls on only once the copy it just restarted is confirmed started again. */
export const cameBack = (legs: readonly ServiceActLeg[], copy: Pick<ServiceActLeg, "machine" | "container">) =>
  legs.every((leg) => leg.machine !== copy.machine || leg.container !== copy.container || !takes(leg, "start") || confirmed(leg.start));

/**
 * The order a run moves copies in: hooks first, since they are only stopped, then the Service's own copies, which a
 * restart rolls one at a time so the rest keep serving.
 */
export const rollOrder = (legs: readonly ServiceActLeg[]) => [...legs.filter((leg) => leg.hook), ...legs.filter((leg) => !leg.hook)];

function haltedAt(legs: readonly ServiceActLeg[]) {
  const started = legs.filter((leg) => takes(leg, "start"));
  const index = started.findIndex((leg) => leg.stop !== null && !confirmed(leg.start));
  const leg = started[index];
  if (leg === undefined) return null;
  return { leg, number: index + 1, untouched: started.slice(index + 1).some((later) => later.stop === null && later.start === null) };
}

/**
 * A Service-wide act never ends done while a Server didn't answer: its copies there may not have moved.
 * A restart that stopped rolling leaves later copies untouched, and those never read as moved.
 */
export function endOf(verb: ServiceVerb, before: ServiceActBefore, legs: readonly ServiceActLeg[], only: ServerRef | null): ServiceActEnd {
  const unanswered = only === null ? before.unanswered : before.unanswered.filter(({ machine }) => machine === only.machine);
  const silent = unanswered.length > 0 ? `${unanswered.map(({ name }) => name).join(", ")} didn't answer.` : null;
  if (legs.length === 0) return silent !== null ? { state: "unknown", reason: silent } : { state: "failed", reason: "No containers to move." };
  const untouched = legs.some((leg) => leg.stop === null && leg.start === null);
  const halted = verb === "restart" ? haltedAt(legs) : null;
  const steps = legs.flatMap((leg) => [leg.stop, leg.start]);
  const reason = halted !== null
    ? [`Container ${halted.number} on ${halted.leg.server} didn't come back.`, halted.leg.reason, halted.untouched ? "The rest are untouched." : null]
      .filter((part) => part !== null).join(" ")
    : legs.find((leg) => leg.reason !== null)?.reason ?? null;
  if (steps.some((step) => step === "unknown" || step === "sent")) return { state: "unknown", reason };
  if (steps.some((step) => step === "failed") || untouched) return { state: "failed", reason };
  return silent !== null ? { state: "unknown", reason: silent } : { state: "done", reason: null };
}

/** A restart counts the copies it starts again, since stopping one is only half of moving it. */
export type ServiceActProgress = {
  readonly id: string;
  readonly verb: ServiceVerb;
  readonly state: ServiceActState;
  readonly total: number;
  readonly done: number;
  readonly reason: string | null;
  /** Whether the run moved only one server's containers, which a retry from the page can't target. */
  readonly scoped: boolean;
};

export function progressOf(act: {
  readonly id: string; readonly verb: ServiceVerb; readonly state: ServiceActState; readonly legs: readonly ServiceActLeg[];
  readonly reason: string | null; readonly only: ServerRef | null;
}): ServiceActProgress {
  const side = act.verb === "stop" ? "stop" : "start";
  const moved = act.legs.filter((leg) => takes(leg, side));
  return { id: act.id, verb: act.verb, state: act.state, total: moved.length, done: moved.filter((leg) => confirmed(leg[side])).length, reason: act.reason, scoped: act.only !== null };
}

const RECEIPTS = { sent: "unknown", unknown: "unknown", done: "yes", already_there: "yes", failed: "no" } as const satisfies Record<LegStep, Receipt>;

export const legOf = (leg: ServiceActLeg): Leg => ({
  server: leg.server,
  stopped: leg.stop === null ? null : RECEIPTS[leg.stop],
  started: leg.start === null ? null : RECEIPTS[leg.start],
  reason: leg.reason,
});

type FrameContainer = { readonly container_id: string; readonly machine_id: string };

/** The part of a Runtime Watch frame that says which Servers run a Service. */
export type ServicesFrame = {
  readonly services: readonly { readonly identity: string; readonly containers: readonly FrameContainer[]; readonly hook_containers: readonly FrameContainer[] }[];
};

/** The copies the Servers report of the Service `identity` (`namespace/runtime`), its hooks included and marked. */
export function copiesIn(frame: ServicesFrame, identity: string) {
  const service = frame.services.find((candidate) => candidate.identity === identity);
  return service === undefined ? [] : [
    ...service.containers.map((copy) => ({ ...copy, hook: false })),
    ...service.hook_containers.map((copy) => ({ ...copy, hook: true })),
  ];
}

export const START_APPROVAL_CAPABILITY = "ployz.restart.start-approval.v1";

const NEEDS_NEWER = "needs a newer Ployz to";

/** Why a Server can't take a run's step: its Ployz is too old. No retry helps until it is upgraded. */
export const needsNewerPloyz = (server: string, side: ServiceVerb) => `${server} ${NEEDS_NEWER} ${side} it.`;

export const needsUpgrade = (reason: string | null) => reason?.includes(` ${NEEDS_NEWER} `) === true;

/**
 * The Server a stop would strand: it runs a copy of the Service but can't take a start approval, so nothing in Ployz
 * Cloud could start the copy again. Null when every such Server can. A hook never starts again, so a Server that holds
 * only hooks strands nothing.
 */
export function strandedByStop(
  copies: readonly { readonly machineId: string; readonly hook: boolean }[],
  machines: readonly { readonly id: string; readonly name: string; readonly capabilities: readonly string[] }[],
): OlderServer | null {
  const hosts = new Set(copies.filter((copy) => !copy.hook).map((copy) => copy.machineId));
  return olderServer(machines.filter((machine) => hosts.has(machine.id)), [START_APPROVAL_CAPABILITY]);
}

/** Why `older` can't take a run's `verb` step, said before anything is sent. */
export const olderStepSays = (older: OlderServer, verb: ServiceVerb) => olderSays(older, (server) => needsNewerPloyz(server, verb));
