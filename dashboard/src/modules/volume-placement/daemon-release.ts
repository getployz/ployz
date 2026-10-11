import { isCurrent } from "#/modules/server-upgrade/server-upgrade";

/** The first daemon release that runs the volume verbs. */
export const PLACEMENT_RELEASE = "0.2.5";

export const runsPlacement = (daemonVersion: string) => isCurrent(daemonVersion, PLACEMENT_RELEASE);

/** A Server as the gate sees it. `storage` null is a Server whose storage the daemon manages, as the planner counts it. */
export type StorageServer = { readonly name: string; readonly storage: "stateless" | "ready" | "pool" | null; readonly daemonVersion: string };

/**
 * The Servers with storage still on an older daemon. A volume run asks every one of them, so placement and restore stay
 * closed for the whole organization until this is empty.
 */
export const placementBlockers = (machines: readonly StorageServer[]) =>
  machines.filter((machine) => machine.storage !== "stateless" && !runsPlacement(machine.daemonVersion)).map((machine) => machine.name);

export const placementUpgradeMessage = (blockers: readonly string[]) =>
  `Every Server with storage must run Ployz ${PLACEMENT_RELEASE} or later. Upgrade ${blockers.join(", ")} first.`;

type PlacementMachine = StorageServer & { readonly id: string; readonly acceptsServices: boolean };

/** The Servers a Volume can move or mirror to: none while any Server with storage blocks placement. */
export const placementServers = (machines: readonly PlacementMachine[]) =>
  placementBlockers(machines).length > 0
    ? new Set<string>()
    : new Set(machines.filter((machine) => machine.acceptsServices && runsPlacement(machine.daemonVersion)).map((machine) => machine.id));

/** Restore seeds the new Volume on the Primary's Server, so its daemon needs SeedVolume, and the run's plan asks every Server with storage. */
export const restoreOpen = (machines: readonly (StorageServer & { readonly id: string })[], primary: string | null) =>
  primary !== null && placementBlockers(machines).length === 0
  && machines.some((machine) => machine.id === primary && runsPlacement(machine.daemonVersion));
