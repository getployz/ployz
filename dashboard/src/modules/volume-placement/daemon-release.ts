import { movedRouting, requirements, serverSupports } from "#/modules/runtime/daemon-compat";

type Daemon = { readonly capabilities: readonly string[] };

/** Every daemon that serves the volume verbs lists its capabilities, so a record that lists none lacks them. */
export const runsPlacement = (machine: Daemon) => machine.capabilities.includes(requirements.volumeMove.capability);

/** A Server as the gate sees it. `storage` null is a Server whose storage the daemon manages, as the planner counts it. */
export type StorageServer = Daemon & { readonly name: string; readonly storage: "stateless" | "ready" | "pool" | null };

/** The Servers with storage that lack the volume verbs: the ones to Upgrade before a Volume can move or mirror. */
export const placementBlockers = (machines: readonly StorageServer[]) =>
  machines.filter((machine) => machine.storage !== "stateless" && !runsPlacement(machine)).map((machine) => machine.name);

export const placementUpgradeMessage = (blockers: readonly string[]) =>
  `Every Server with storage needs a newer Ployz. Upgrade ${blockers.join(", ")} first.`;

/** Checks where a copy's data began, so it may hold a born Volume. A Legacy Volume needs no such check. */
export const checksOrigins = (machine: Daemon) => machine.capabilities.includes(requirements.volumeOrigin.capability);

/** Which of `servers` can't hold a born Volume, by name; one the frame doesn't show, by ID. */
export const originBlockers = (machines: readonly (Daemon & { readonly id: string; readonly name: string })[], servers: readonly string[]) =>
  [...new Set(servers)].flatMap((id) => {
    const machine = machines.find((entry) => entry.id === id);
    if (machine === undefined) return [id];
    return checksOrigins(machine) ? [] : [machine.name];
  });

export const originUpgradeMessage = (volume: string, blockers: readonly string[]) =>
  `Upgrade ${blockers.join(", ")} first. ${volume} needs a newer Ployz on every Server that holds it.`;

type PlacementMachine = StorageServer & { readonly id: string; readonly acceptsServices: boolean };

/** The Servers a Volume can move or mirror to: none while any Server with storage blocks placement. */
export const placementServers = (machines: readonly PlacementMachine[]) =>
  placementBlockers(machines).length > 0
    ? new Set<string>()
    : new Set(machines.filter((machine) => machine.acceptsServices && runsPlacement(machine)).map((machine) => machine.id));

/** A Move onto it finishes: its Ployz routes the moved copy once it starts it. */
const finishesMoves = (machine: Daemon) => serverSupports(machine, movedRouting) !== false;

/** The Servers a Volume can move to: `placementServers` without the ones whose Ployz takes the data, then never routes the copy. */
export const moveServers = (machines: readonly PlacementMachine[]) => {
  const placeable = placementServers(machines);
  return new Set(machines.filter((machine) => placeable.has(machine.id) && finishesMoves(machine)).map((machine) => machine.id));
};

/** The Servers `moveServers` leaves out: a Volume can move there once they're upgraded. */
export const unroutedServers = (machines: readonly PlacementMachine[]) => {
  const placeable = placementServers(machines);
  return new Set(machines.filter((machine) => placeable.has(machine.id) && !finishesMoves(machine)).map((machine) => machine.id));
};

export const restoreOpen = (machines: readonly (StorageServer & { readonly id: string })[], primary: string | null) =>
  primary !== null && placementBlockers(machines).length === 0
  && machines.some((machine) => machine.id === primary && runsPlacement(machine));
