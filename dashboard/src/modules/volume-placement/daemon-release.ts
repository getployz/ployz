import { isCurrent } from "#/modules/server-upgrade/server-upgrade";

/** The first release whose daemon has the volume verbs Placement and Restore call: mirror, move, restore and seed. */
export const PLACEMENT_RELEASE = "0.2.5";

/** A Server's daemon has those verbs. An unknown version doesn't, so Cloud hides what needs them until it upgrades. */
export const runsPlacement = (daemonVersion: string) => isCurrent(daemonVersion, PLACEMENT_RELEASE);

type PlacementMachine = { readonly id: string; readonly acceptsServices: boolean; readonly daemonVersion: string };

/** Servers a move or a mirror may name: they accept Services and their daemon has the volume verbs. */
export const placementServers = (machines: readonly PlacementMachine[]) =>
  new Set(machines.filter((machine) => machine.acceptsServices && runsPlacement(machine.daemonVersion)).map((machine) => machine.id));
