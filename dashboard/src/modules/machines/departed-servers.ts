import type { MachineId, RuntimeWatchView } from "@ployz/sdk";

/**
 * How long a Server Cloud holds a row for may go unlisted before Cloud reads it as departed: its row is saved only
 * once it joined, so this covers replication reaching the Server a frame was read from.
 */
export const DEPARTED_GRACE_MS = 5 * 60_000;

type Held = { readonly machineId: MachineId; readonly updatedAt: Date };

/**
 * The Servers Cloud holds a row for that the Cluster no longer lists, as a person removed them from an approved device
 * without Cloud. Only a frame in which every listed Server reads up counts, so a partitioned entry, or one still
 * catching up, drops nothing. `held` are rows Cloud's own removal runs on, or saved within the grace: they wait.
 */
export function departedServers(
  rows: readonly Held[],
  frame: Pick<RuntimeWatchView, "machines">,
  now: Date,
  removing: ReadonlySet<string>,
) {
  const complete = frame.machines.length > 0 && frame.machines.every(({ membership }) => membership === "up");
  const listed = new Set<string>(frame.machines.map(({ machine }) => machine.id));
  const unlisted = complete ? rows.filter((row) => !listed.has(row.machineId)) : [];
  const waits = (row: Held) => removing.has(row.machineId) || now.getTime() - row.updatedAt.getTime() < DEPARTED_GRACE_MS;
  return {
    departed: unlisted.filter((row) => !waits(row)).map((row) => row.machineId),
    held: unlisted.filter(waits).map((row) => row.machineId),
  };
}

/** What `departedServers` reads of a frame: who is listed, and whether each reads up. */
export const runtimeMembershipDigest = (frame: Pick<RuntimeWatchView, "machines">) =>
  JSON.stringify(frame.machines.map(({ machine, membership }) => [machine.id, membership]));
