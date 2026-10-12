import { requirements, serverSupports } from "#/modules/runtime/daemon-compat";

export const runsPlacement = (machine: { readonly capabilities: readonly string[] }) => serverSupports(machine, requirements.volumeMove) === true;

/** A Server as the gate sees it. `storage` null is a Server whose storage the daemon manages, as the planner counts it. */
export type StorageServer = { readonly name: string; readonly storage: "stateless" | "ready" | "pool" | null; readonly capabilities: readonly string[] };

export const placementBlockers = (machines: readonly StorageServer[]) =>
  machines.filter((machine) => machine.storage !== "stateless" && !runsPlacement(machine)).map((machine) => machine.name);

const answering = (machines: readonly StorageServer[], answer: false | "unknown") =>
  machines.filter((machine) => machine.storage !== "stateless" && serverSupports(machine, requirements.volumeMove) === answer).map((machine) => machine.name);

/** The blockers whose records say they lack the verbs: the only ones an Upgrade is claimed to fix. */
export const placementLacking = (machines: readonly StorageServer[]) => answering(machines, false);

/** The blockers whose records list no capabilities, so Ployz cannot tell whether they serve the verbs. */
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
