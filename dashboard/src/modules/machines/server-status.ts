import { useEffect, useState } from "react";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";

export type ServerStatus = "online" | "building" | "not_responding" | "offline" | "unknown";

/** A network blip leaves a Server `suspect` for seconds, so it reads Not responding only after a minute of it. */
export const NOT_RESPONDING_AFTER_MS = 60_000;

/**
 * The one word a Server's status shows. Membership decides it; a Server that answers reads Building while it
 * runs builds. `suspectForMs` is how long the Server has been `suspect`.
 */
export function serverStatus(
  machine: Pick<RuntimeMachineRecord, "membership" | "runningBuilds">,
  suspectForMs: number,
): ServerStatus {
  switch (machine.membership) {
    case "suspect":
      if (suspectForMs >= NOT_RESPONDING_AFTER_MS) return "not_responding";
      return machine.runningBuilds > 0 ? "building" : "online";
    case "up":
      return machine.runningBuilds > 0 ? "building" : "online";
    case "down":
      return "offline";
    default:
      return "unknown";
  }
}

const RANK = { offline: 0, not_responding: 1, unknown: 2, building: 3, online: 4 } satisfies Record<ServerStatus, number>;

/** Servers that need someone come first, then by name. */
export function sortServers<T extends { name: string; status: ServerStatus }>(servers: readonly T[]): T[] {
  return [...servers].sort((left, right) =>
    RANK[left.status] - RANK[right.status] || left.name.localeCompare(right.name, undefined, { numeric: true }));
}

/**
 * Each Server's status, remembering when it was first seen `suspect`.
 * ponytail: the minute restarts when a page mounts; keep first-seen times in the Runtime provider if that matters.
 */
export function useServerStatuses(machines: readonly RuntimeMachineRecord[]) {
  const suspect = machines.filter((machine) => machine.membership === "suspect").map((machine) => machine.id).join(" ");
  const [firstSeen, setFirstSeen] = useState<ReadonlyMap<string, number>>(() => new Map());
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const seenAt = Date.now();
    setFirstSeen((previous) => new Map(suspect.split(" ").filter(Boolean).map((id) => [id, previous.get(id) ?? seenAt])));
    setNow(seenAt);
    if (suspect === "") return;
    const timer = setInterval(() => setNow(Date.now()), 5_000);
    return () => clearInterval(timer);
  }, [suspect]);
  return (machine: RuntimeMachineRecord) => serverStatus(machine, now - (firstSeen.get(machine.id) ?? now));
}
