import type { RuntimeMachineRecord, RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { ACTIVE_VOLUME_RUN_STATES, copyName, parseDockerVolumeName, type VolumeRunState, type VolumeRunView } from "./volume-run";

/** What a copy is to the user. `old` is a writer that lost: demoted, or another Server holds the newer one. */
export type CopyRole = "writer" | "mirror" | "moving" | "old";

export type VolumeCopy = {
  readonly server: string;
  readonly machineId: string;
  /** `data` for the writer, `data-web-2` for any other copy. */
  readonly label: string;
  readonly role: CopyRole;
  readonly online: boolean;
};

/** A run the panel can start, with the Servers it can name; null hides it. */
export type Offer = { readonly servers: readonly string[] } | null;

export type VolumeCopies = {
  readonly copies: readonly VolumeCopy[];
  /** The run in progress. While one runs, nothing else can start. */
  readonly active: VolumeRunView | null;
  /** Two writers and no run says which is newer. */
  readonly twoWriters: boolean;
  readonly offers: {
    readonly mirror: Offer;
    readonly sync: Offer;
    readonly move: Offer;
    readonly release: Offer;
    readonly restore: Offer;
  };
};

const ENGINE_ROLE = { writer: "writer", slot: "mirror", switching: "moving", old: "old" } as const satisfies Record<RuntimeVolumeCopy["role"], CopyRole>;
const RELEASABLE: readonly VolumeRunState[] = ["failed", "lost", "cancelled"];
const isActive = (run: VolumeRunView) => ACTIVE_VOLUME_RUN_STATES.some((state) => state === run.state);
const isOnline = (machine: Pick<RuntimeMachineRecord, "membership"> | undefined) => machine?.membership === "up" || machine?.membership === "suspect";

/** Any run's arguments, each field present only for the kinds that take it. */
function runArgs(run: VolumeRunView): Partial<{ readonly to: string; readonly from: string; readonly full: boolean; readonly slot: string | null }> {
  return run.args;
}

/** The Server the last finished Move or Restore made the writer: of two writers, the other one is old. */
function lastWriter(runs: readonly VolumeRunView[]): string | null {
  for (const run of runs) {
    if (run.state !== "done") continue;
    const args = runArgs(run);
    if (run.kind === "move" && args.to !== undefined) return args.to;
    if (run.kind === "restore" && args.from !== undefined) return args.from;
  }
  return null;
}

/**
 * A Volume's copies as the Servers report them, and which runs the panel offers. Machines decide every step; this
 * only chooses what to show, and Cloud refuses a run whose copies changed since.
 */
export function volumeCopies(input: {
  readonly volume: { readonly id: string; readonly name: string };
  readonly copies: readonly RuntimeVolumeCopy[];
  readonly machines: readonly Pick<RuntimeMachineRecord, "id" | "name" | "membership" | "storage" | "acceptsServices">[];
  /** Newest first. */
  readonly runs: readonly VolumeRunView[];
}): VolumeCopies {
  const machines = new Map(input.machines.map((machine) => [machine.id, machine]));
  const observed = input.copies
    .filter((copy) => parseDockerVolumeName(copy.name)?.volumeId === input.volume.id)
    .map((copy) => {
      const machine = machines.get(copy.machineId);
      return { server: machine?.name ?? copy.machineId, machineId: copy.machineId, role: ENGINE_ROLE[copy.role], online: isOnline(machine) };
    });

  const writers = observed.filter((copy) => copy.role === "writer");
  const newest = writers.length > 1 ? lastWriter(input.runs) : null;
  const known = newest !== null && writers.some((copy) => copy.server === newest);
  const copies = observed
    .map((copy) => (known && copy.role === "writer" && copy.server !== newest ? { ...copy, role: "old" as const } : copy))
    .map((copy) => ({ ...copy, label: copy.role === "writer" ? input.volume.name : copyName(input.volume.name, copy.server) }))
    .sort((left, right) => rank(left.role) - rank(right.role) || left.server.localeCompare(right.server, undefined, { numeric: true }));

  const active = input.runs.find(isActive) ?? null;
  const twoWriters = copies.filter((copy) => copy.role === "writer").length > 1;
  const none: VolumeCopies["offers"] = { mirror: null, sync: null, move: null, release: null, restore: null };
  if (active !== null || twoWriters) return { copies, active, twoWriters, offers: none };

  const writer = copies.find((copy) => copy.role === "writer" && copy.online);
  const mirrors = copies.filter((copy) => copy.role === "mirror");
  const holders = new Set(copies.map((copy) => copy.machineId));
  const managed = input.machines
    .filter((machine) => isOnline(machine) && machine.acceptsServices && (machine.storage === "ready" || machine.storage === "pool"))
    .map((machine) => machine.id);
  const names = (ids: readonly string[]) => ids.map((id) => machines.get(id)?.name ?? id).sort((left, right) => left.localeCompare(right, undefined, { numeric: true }));
  const offer = (servers: readonly string[]): Offer => (servers.length > 0 ? { servers } : null);
  const latest = input.runs[0];

  return {
    copies,
    active,
    twoWriters,
    offers: writer === undefined ? {
      ...none,
      restore: copies.some((copy) => copy.role === "writer") ? null : offer(mirrors.filter((copy) => copy.online).map((copy) => copy.server)),
      release: latest?.kind === "move" && RELEASABLE.includes(latest.state) ? { servers: [] } : null,
    } : {
      mirror: mirrors.length === 0 ? offer(names(managed.filter((id) => !holders.has(id)))) : null,
      sync: mirrors.length > 0 ? { servers: [] } : null,
      move: offer(names(managed.filter((id) => id !== writer.machineId))),
      release: latest?.kind === "move" && RELEASABLE.includes(latest.state) ? { servers: [] } : null,
      restore: null,
    },
  };
}

function rank(role: CopyRole) {
  return { writer: 0, moving: 1, mirror: 2, old: 3 }[role];
}

const STATE_WORD = {
  requested: "Starting",
  running: "Running",
  done: "Done",
  failed: "Failed",
  cancelled: "Cancelled",
  lost: "Lost",
  not_started: "Not started",
} as const satisfies Record<VolumeRunState, string>;

/** A run as one line of activity: "Move to web-2", and its state in one word. */
export function runText(run: VolumeRunView) {
  const args = runArgs(run);
  const what = {
    mirror: `Mirror to ${args.to}`,
    sync: args.full ? "Full sync" : "Sync",
    delete_mirror: args.slot ? `Delete mirror ${copyName(run.volume_name, args.slot)}` : "Delete mirror",
    move: `Move to ${args.to}`,
    release: "Release",
    restore: `Restore from ${args.from}`,
  }[run.kind];
  return { what, state: STATE_WORD[run.state] };
}
