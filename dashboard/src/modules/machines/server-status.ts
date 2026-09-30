import type { RuntimeLensStatus, RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";

export type ServerStatus = "online" | "building" | "offline" | "unknown";

/** Storage support as observed, separate from whether the Server is online. */
export function volumeSupportText(storage: RuntimeMachineRecord["storage"]) {
  return storage === null ? "Volume support unknown"
    : storage === "stateless" ? "Docker only" : "Managed volumes available";
}

/**
 * The one word a Server's status shows. A `suspect` Server still reads online: SWIM settles suspicion to up or
 * down within seconds (at most ~30s on a small cluster), so a network blip never shows and an outage reads Offline.
 */
export function serverStatus(machine: Pick<RuntimeMachineRecord, "membership" | "runningBuilds">): ServerStatus {
  switch (machine.membership) {
    case "up":
    case "suspect":
      return machine.runningBuilds > 0 ? "building" : "online";
    case "down":
      return "offline";
    default:
      return "unknown";
  }
}

const RANK = { offline: 0, unknown: 1, building: 2, online: 2 } satisfies Record<ServerStatus, number>;

/** Offline and Unknown need someone; a Server that is building is online. */
export const needsAttention = (status: ServerStatus) => RANK[status] < RANK.online;

/** Servers that need someone come first, then by name. */
export function sortServers<T extends { name: string; status: ServerStatus }>(servers: readonly T[]): T[] {
  return [...servers].sort((left, right) =>
    RANK[left.status] - RANK[right.status] || left.name.localeCompare(right.name, undefined, { numeric: true }));
}

/**
 * What the Servers pages can show for a Runtime Watch status. `unavailable` keeps the last observation, if any; an
 * incomplete observation is just as uncertain (incomplete IDs are never deletions), as the Projects page reads it too.
 */
export function serverListState(status: RuntimeLensStatus, serverCount: number, incomplete: boolean) {
  const uncertain = serverCount > 0 ? "stale" : "unreachable";
  switch (status) {
    case "connecting":
      return "loading";
    case "unreachable":
      return "unreachable";
    case "unavailable":
      return uncertain;
    case "observed":
      return incomplete ? uncertain : "live";
    case "no_connection":
      return "live";
    default: {
      const _exhaustive: never = status;
      return _exhaustive;
    }
  }
}
