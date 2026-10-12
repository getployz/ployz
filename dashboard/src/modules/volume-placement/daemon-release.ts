import { requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { compareVersions } from "#/modules/server-upgrade/server-upgrade";

/** The first release that runs the volume verbs, for a daemon too old to list its capabilities. */
const PLACEMENT_RELEASE = "0.2.5";

type Daemon = { readonly daemonVersion: string; readonly capabilities: readonly string[] };

const placementSupport = (machine: Daemon): boolean | "unknown" => {
  const listed = serverSupports(machine, requirements.volumeMove);
  if (listed !== "unknown") return listed;
  const order = compareVersions(machine.daemonVersion, PLACEMENT_RELEASE);
  return order === null ? "unknown" : order >= 0;
};

export const runsPlacement = (machine: Daemon) => placementSupport(machine) === true;

/** A Server as the gate sees it. `storage` null is a Server whose storage the daemon manages, as the planner counts it. */
export type StorageServer = Daemon & { readonly name: string; readonly storage: "stateless" | "ready" | "pool" | null };

export const placementBlockers = (machines: readonly StorageServer[]) =>
  machines.filter((machine) => machine.storage !== "stateless" && !runsPlacement(machine)).map((machine) => machine.name);

const answering = (machines: readonly StorageServer[], answer: false | "unknown") =>
  machines.filter((machine) => machine.storage !== "stateless" && placementSupport(machine) === answer).map((machine) => machine.name);

/** The blockers whose records say they lack the verbs: the only ones an Upgrade is claimed to fix. */
export const placementLacking = (machines: readonly StorageServer[]) => answering(machines, false);

/** The blockers whose records list no capabilities and no version Ployz can read, so it cannot tell whether they serve the verbs. */
export const placementUnknown = (machines: readonly StorageServer[]) => answering(machines, "unknown");

export const placementUpgradeMessage = (blockers: readonly string[]) =>
  `Every Server with storage needs a newer Ployz. Upgrade ${blockers.join(", ")} first.`;

export const placementUnknownMessage = (servers: readonly string[]) =>
  `Ployz can't tell whether ${servers.join(", ")} can move volumes.`;

type PlacementMachine = StorageServer & { readonly id: string; readonly acceptsServices: boolean };

/** The Servers a Volume can move or mirror to: none while any Server with storage blocks placement. */
export const placementServers = (machines: readonly PlacementMachine[]) =>
  placementBlockers(machines).length > 0
    ? new Set<string>()
    : new Set(machines.filter((machine) => machine.acceptsServices && runsPlacement(machine)).map((machine) => machine.id));

export const restoreOpen = (machines: readonly (StorageServer & { readonly id: string })[], primary: string | null) =>
  primary !== null && placementBlockers(machines).length === 0
  && machines.some((machine) => machine.id === primary && runsPlacement(machine));
