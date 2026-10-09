import type { RuntimeMachineRecord, RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { type Planned, planFromCopies, type Role, roleOf } from "./plan";
import {
  ACTIVE_VOLUME_RUN_STATES,
  type AnsweredMember,
  copyName,
  type Member,
  parseDockerVolumeName,
  type VolumeRunInput,
  type VolumeRunState,
  type VolumeRunView,
} from "./volume-run";

/** What a copy is to the user. `old` is a writer that lost: Cloud found a newer one on another Server. */
export type CopyRole = "writer" | "mirror" | "moving" | "old";

export type VolumeCopy = {
  readonly server: string;
  readonly machineId: string;
  /** `data` for the writer, `data-web-2` for any other copy. */
  readonly label: string;
  readonly role: CopyRole;
};

/** A run the panel can start, with the Servers it can name; null hides it. */
export type Offer = { readonly servers: readonly string[] } | null;

export type VolumeCopies = {
  readonly copies: readonly VolumeCopy[];
  /** Servers that did not answer: their copies are unknown, and Cloud plans no run until they answer. */
  readonly unanswered: readonly string[];
  /** The run in progress. While one runs, nothing else can start. */
  readonly active: VolumeRunView | null;
  /** Two writers whose records leave no way to tell which is newer. */
  readonly twoWriters: boolean;
  /** An old copy whose Server still serves it as the writer: the next run makes it read-only first, then stops. */
  readonly sealing: string | null;
  readonly offers: {
    readonly mirror: Offer;
    readonly sync: Offer;
    readonly move: Offer;
    readonly release: Offer;
    readonly restore: Offer;
  };
};

const PANEL_ROLE = {
  writer: "writer",
  switching: "moving",
  handed: "moving",
  mirror: "mirror",
  stale: "moving",
} as const satisfies Record<Exclude<Role, "empty" | "unanswered">, CopyRole>;
const isActive = (run: VolumeRunView) => ACTIVE_VOLUME_RUN_STATES.some((state) => state === run.state);
const byName = (left: string, right: string) => left.localeCompare(right, undefined, { numeric: true });

/** Any run's arguments, each field present only for the kinds that take it. */
function runArgs(run: VolumeRunView): Partial<{ readonly to: string; readonly from: string; readonly full: boolean; readonly slot: string | null }> {
  return run.args;
}

/**
 * A Volume's copies as each Server answers for them, and the runs the panel offers. Every offer is a run Cloud's planner
 * plans from these same answers, so the panel offers nothing Cloud would refuse; Cloud plans again when the run starts.
 */
export function volumeCopies(input: {
  readonly volume: { readonly name: string };
  readonly members: readonly Member[];
  /** Servers the runtime says take Services; Mirror and Move name only those. */
  readonly machines: readonly Pick<RuntimeMachineRecord, "id" | "acceptsServices">[];
  /** Newest first. */
  readonly runs: readonly VolumeRunView[];
}): VolumeCopies {
  const name = input.volume.name;
  const plan = (run: VolumeRunInput, members: readonly Member[] = input.members) => planFromCopies({ ...run, volumeName: name, orphan: false }, members);
  const probe = plan({ kind: "sync", args: { full: false } });
  const demote = probe.ok && probe.phase.kind === "demote" ? probe.phase : null;
  const twoWriters = !probe.ok && probe.refusal.code === "two_writers";
  const answered = input.members.filter((member): member is AnsweredMember => member.answered);
  const copies = answered.flatMap((member): VolumeCopy[] => {
    const role = roleOf(member);
    if (role === "empty" || role === "unanswered") return [];
    const shown = member === demote?.old ? "old" : PANEL_ROLE[role];
    const server = member.machine.name;
    return [{ server, machineId: member.machine.id, role: shown, label: shown === "writer" ? name : copyName(name, server) }];
  }).sort((left, right) => rank(left.role) - rank(right.role) || byName(left.server, right.server));
  const unanswered = input.members.filter((member) => !member.answered).map((member) => member.machine.name).sort(byName);

  const active = input.runs.find(isActive) ?? null;
  const sealing = demote === null ? null : copyName(name, demote.old.machine.name);
  const none: VolumeCopies["offers"] = { mirror: null, sync: null, move: null, release: null, restore: null };
  if (active !== null || twoWriters) return { copies, unanswered, active, twoWriters, sealing, offers: none };

  // A run that finds an old copy only makes it read-only, then asks to run again; offer what that second run plans.
  const basis = demote === null ? input.members : input.members.filter((member) => member !== demote.old);
  const takesServices = new Set(input.machines.filter((machine) => machine.acceptsServices).map((machine) => machine.id));
  const servers = (accept: (member: AnsweredMember) => boolean) => answered.filter(accept).map((member) => member.machine.name).sort(byName);
  const offer = (names: readonly string[]): Offer => (names.length > 0 ? { servers: names } : null);
  const phase = (planned: Planned) => (planned.ok ? planned.phase : null);
  const targets = answered.filter((member) => takesServices.has(member.machine.id));
  const onTo = (kind: "mirror" | "move", accept: (planned: Planned) => boolean) =>
    offer(servers((member) => targets.includes(member) && accept(plan({ kind, args: { to: member.machine.name } }, basis))));
  const release = phase(plan({ kind: "release", args: {} }, basis));

  return {
    copies,
    unanswered,
    active,
    twoWriters,
    sealing,
    offers: {
      // Mirror to a Server that holds no mirror yet; refreshing the one there is Sync.
      mirror: onTo("mirror", (planned) => { const next = phase(planned); return next?.kind === "mirror" && next.declare; }),
      sync: phase(plan({ kind: "sync", args: { full: false } }, basis))?.kind === "sync" ? { servers: [] } : null,
      // Undo only thaws the source and stops, which is what Release offers.
      move: onTo("move", (planned) => { const next = phase(planned); return next?.kind === "move" && next.start !== "undo"; }),
      // Release with nothing frozen and no final mirror would do nothing.
      release: release?.kind === "release" && (release.thaw || release.mirror !== null) ? { servers: [] } : null,
      restore: offer(servers((member) => phase(plan({ kind: "restore", args: { from: member.machine.name } }, basis))?.kind === "restore")),
    },
  };
}

function rank(role: CopyRole) {
  return { writer: 0, moving: 1, mirror: 2, old: 3 }[role];
}

export type TrayCopy = { readonly kind: "moving"; readonly text: string } | { readonly kind: "mirror"; readonly label: string } | null;

/**
 * What a Volume's canvas tray says about its copies: the Move in progress by the target its run names, a copy switching
 * with no run as a bare "Moving", else the mirror by name.
 */
export function trayCopy(input: {
  readonly volume: { readonly id: string; readonly name: string };
  readonly copies: readonly RuntimeVolumeCopy[];
  readonly machines: readonly Pick<RuntimeMachineRecord, "id" | "name">[];
  readonly active: VolumeRunView | null;
}): TrayCopy {
  const own = input.copies.filter((copy) => parseDockerVolumeName(copy.name)?.volumeId === input.volume.id);
  const to = input.active?.kind === "move" ? runArgs(input.active).to : undefined;
  if (to !== undefined) return { kind: "moving", text: `Moving to ${to}` };
  if (own.some((copy) => copy.role === "switching")) return { kind: "moving", text: "Moving" };
  const mirror = own.find((copy) => copy.role === "slot");
  if (mirror === undefined) return null;
  const server = input.machines.find((machine) => machine.id === mirror.machineId)?.name ?? mirror.machineId;
  return { kind: "mirror", label: copyName(input.volume.name, server) };
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
