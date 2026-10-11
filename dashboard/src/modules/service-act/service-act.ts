import type { ContainerState, Leg, Receipt, RunState, ServerRef, ServiceVerb } from "@ployz/sdk";

/**
 * One restart, start or stop of a Service's Containers, as its row moves: the request writes `accepted`, the run's
 * claim reads the Containers and writes `running`, and the last step ends it `done`, `failed` or `unknown`.
 */
export const SERVICE_ACT_STATES = ["accepted", "running", "done", "failed", "unknown"] as const satisfies readonly RunState[];
export type ServiceActState = (typeof SERVICE_ACT_STATES)[number];
export const SERVICE_ACT_VERBS = ["restart", "start", "stop"] as const satisfies readonly ServiceVerb[];
export type ActiveServiceActState = Extract<ServiceActState, "accepted" | "running">;

/** One Container's step: `sent` is written before the Server is asked, so a retry that finds it never asks again. */
export type LegStep = "sent" | "done" | "already_there" | "failed" | "unknown";

export type ServiceCopy = { server: string; machine: string; container: string; state: ContainerState };
/** The Service's Containers when the run claimed it, and the Servers that didn't answer. */
export type ServiceActBefore = { copies: ServiceCopy[]; unanswered: string[] };
export type ServiceActLeg = {
  server: string;
  machine: string;
  container: string;
  stop: LegStep | null;
  start: LegStep | null;
  reason: string | null;
};

export const SERVICE_ACT_PENDING_LIMIT_MS = 15 * 60_000;
export const SERVICE_ACT_RUNNING_LIMIT_MS = 60 * 60_000;

const STOPPED: ReadonlySet<ContainerState> = new Set(["created", "exited", "dead"]);

export const stepsOf = (verb: ServiceVerb) => ({ stop: verb !== "start", start: verb !== "stop" });

/** The Containers the act moves: every copy, or only those on `only`. */
export const legsOf = (before: ServiceActBefore, only: ServerRef | null): ServiceActLeg[] =>
  before.copies
    .filter((copy) => only === null || copy.machine === only.machine)
    .map(({ server, machine, container }) => ({ server, machine, container, stop: null, start: null, reason: null }));

/** A stop of a Container that isn't running, or a lone start of one that is, has nothing to do. */
export const alreadyThere = (verb: ServiceVerb, side: "stop" | "start", state: ContainerState | undefined) =>
  side === "stop" ? state !== undefined && STOPPED.has(state) : verb === "start" && state === "running";

/** Start follows a stop only when the stop is known to have happened. */
export const mayStart = (verb: ServiceVerb, leg: ServiceActLeg) =>
  verb === "start" || leg.stop === "done" || leg.stop === "already_there";

export const serversOf = (legs: readonly ServiceActLeg[]) => [...new Set(legs.map(({ server }) => server))];

/** How the act ended, from its legs. With nothing to move, Servers that didn't answer leave it unknown. */
export function endOf(before: ServiceActBefore, legs: readonly ServiceActLeg[]): {
  state: Exclude<ServiceActState, ActiveServiceActState>;
  reason: string | null;
} {
  if (legs.length === 0) {
    return before.unanswered.length > 0
      ? { state: "unknown", reason: `${before.unanswered.join(", ")} didn't answer.` }
      : { state: "failed", reason: "No containers to move." };
  }
  const steps = legs.flatMap((leg) => [leg.stop, leg.start]);
  const reason = legs.find((leg) => leg.reason !== null)?.reason ?? null;
  if (steps.some((step) => step === "unknown" || step === "sent")) return { state: "unknown", reason };
  if (steps.some((step) => step === "failed")) return { state: "failed", reason };
  return { state: "done", reason: null };
}

const RECEIPTS: Record<LegStep, Receipt> = { sent: "unknown", unknown: "unknown", done: "yes", already_there: "yes", failed: "no" };

export const legOf = (leg: ServiceActLeg): Leg => ({
  server: leg.server,
  stopped: leg.stop === null ? null : RECEIPTS[leg.stop],
  started: leg.start === null ? null : RECEIPTS[leg.start],
  reason: leg.reason,
});
