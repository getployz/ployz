import type { ContainerState, Leg, Receipt, RunState, ServerRef, ServiceVerb } from "@ployz/sdk";

export const SERVICE_ACT_STATES = ["accepted", "running", "done", "failed", "unknown"] as const satisfies readonly RunState[];
export type ServiceActState = (typeof SERVICE_ACT_STATES)[number];
export const SERVICE_ACT_VERBS = ["restart", "start", "stop"] as const satisfies readonly ServiceVerb[];
export type ActiveServiceActState = Extract<ServiceActState, "accepted" | "running">;

export type LegStep = "sent" | "done" | "already_there" | "failed" | "unknown";

export type ServiceCopy = { server: string; machine: string; container: string; state: ContainerState };
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

export const legsOf = (before: ServiceActBefore, only: ServerRef | null): ServiceActLeg[] =>
  before.copies
    .filter((copy) => only === null || copy.machine === only.machine)
    .map(({ server, machine, container }) => ({ server, machine, container, stop: null, start: null, reason: null }));

export const alreadyThere = (verb: ServiceVerb, side: "stop" | "start", state: ContainerState | undefined) =>
  side === "stop" ? state !== undefined && STOPPED.has(state) : verb === "start" && state === "running";

export const serversOf = (legs: readonly ServiceActLeg[]) => [...new Set(legs.map(({ server }) => server))];

export type ServiceActEnd = { state: Exclude<ServiceActState, ActiveServiceActState>; reason: string | null };

/** A Service-wide act never ends done while a Server didn't answer: its copies there may not have moved. */
export function endOf(before: ServiceActBefore, legs: readonly ServiceActLeg[], only: ServerRef | null): ServiceActEnd {
  const unanswered = only === null ? before.unanswered : before.unanswered.filter((name) => name === only.name);
  const silent = unanswered.length > 0 ? `${unanswered.join(", ")} didn't answer.` : null;
  if (legs.length === 0) return silent !== null ? { state: "unknown", reason: silent } : { state: "failed", reason: "No containers to move." };
  const steps = legs.flatMap((leg) => [leg.stop, leg.start]);
  const reason = legs.find((leg) => leg.reason !== null)?.reason ?? null;
  if (steps.some((step) => step === "unknown" || step === "sent")) return { state: "unknown", reason };
  if (steps.some((step) => step === "failed")) return { state: "failed", reason };
  return silent !== null ? { state: "unknown", reason: silent } : { state: "done", reason: null };
}

const RECEIPTS = { sent: "unknown", unknown: "unknown", done: "yes", already_there: "yes", failed: "no" } as const satisfies Record<LegStep, Receipt>;

export const legOf = (leg: ServiceActLeg): Leg => ({
  server: leg.server,
  stopped: leg.stop === null ? null : RECEIPTS[leg.stop],
  started: leg.start === null ? null : RECEIPTS[leg.start],
  reason: leg.reason,
});
