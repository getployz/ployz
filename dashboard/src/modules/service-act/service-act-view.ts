import type { ServiceVerb } from "@ployz/sdk";
import type { ServiceActProgress } from "./service-act";

const MOVING = { restart: "Restarting", stop: "Stopping", start: "Starting" } as const satisfies Record<ServiceVerb, string>;
const HALTED = { restart: "Restart stopped", stop: "Stop failed", start: "Start failed" } as const satisfies Record<ServiceVerb, string>;

const running = (act: ServiceActProgress) => act.state === "accepted" || act.state === "running";

/**
 * What a Service's header says while a run moves its containers, or null when none does. `requested` is a click whose
 * row hasn't arrived yet: it reads as started at once, with no count until the row says how many containers move.
 * Only a restart counts, since it is the one that takes long: one container at a time.
 */
export function actBadge(latest: ServiceActProgress | null | undefined, requested: ServiceVerb | null): string | null {
  if (requested !== null) return `${MOVING[requested]}…`;
  if (!latest || !running(latest)) return null;
  return latest.verb === "restart" && latest.total > 0 ? `Restarting ${latest.done}/${latest.total}` : `${MOVING[latest.verb]}…`;
}

/** Why the Service's latest run ended short of done, with the verb Retry runs again; null while it runs or once it ended done. */
export function actAlert(latest: ServiceActProgress | null | undefined) {
  if (!latest || running(latest) || latest.state === "done") return null;
  return { title: HALTED[latest.verb], description: latest.reason ?? "A server didn't answer.", verb: latest.verb };
}
