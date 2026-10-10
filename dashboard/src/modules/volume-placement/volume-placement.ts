import type { ChangeKind, MachineId, MachineName } from "@ployz/sdk";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { planFromCopies } from "#/modules/volume-run/plan";
import { type AnsweredMember, copyName, type Member, type VolumeRunInput } from "#/modules/volume-run/volume-run";

export type MachineRef = { readonly id: MachineId; readonly name: MachineName };
export type PlacementVolume = { readonly id: string; readonly name: string };

/** A copy other than the primary, as its Server reports it. A `root` is an old primary that came back. */
export type ObservedCopy = { readonly server: MachineRef; readonly kind: "slot" | "root"; readonly newest: Date | null };

/** What the Servers report, in the user's words: the primary is the writer, every other copy is a copy. */
export type ObservedPlacement = {
  /** Null when no writer answered: its Server is down, or the Volume was never deployed. */
  readonly primary: MachineRef | null;
  readonly copies: readonly ObservedCopy[];
  readonly unanswered: readonly MachineRef[];
};

/** Where the user wants the Volume. `deletes` names detached copies to delete. */
export type Placement = { readonly primary: MachineId; readonly mirrors: readonly MachineId[]; readonly deletes: readonly MachineId[] };

/**
 * The dashboard's record of a Volume's placement. `mirrors` are the copies that refresh; a copy on any other Server is
 * a detached volume. Null `mirrors` means no record yet, so every copy counts as a mirror, as the CLI made them.
 */
export type PlacementRow = {
  readonly volumeId: string;
  readonly environmentId: string;
  readonly mirrors: readonly MachineId[] | null;
  readonly staged: Placement | null;
  /** The Deploy that owns `staged`; Discard waits for it. */
  readonly deploymentId: string | null;
};

export type MoveHow = "promote" | "copy" | "restore";

/** One row of the review. A move is one change, whatever it does to Mirrors. */
export type PlacementChange =
  | {
    readonly kind: "move";
    readonly volume: PlacementVolume;
    readonly from: MachineRef | null;
    readonly to: MachineRef;
    readonly how: MoveHow;
    /** Restore only: writes after this time are lost. */
    readonly lostAfter: Date | null;
    /** Detached copies the move deletes first, since a Volume holds one copy besides its primary for now. */
    readonly clears: readonly MachineRef[];
  }
  | { readonly kind: "add_mirror"; readonly volume: PlacementVolume; readonly server: MachineRef; readonly clears: readonly MachineRef[] }
  | { readonly kind: "remove_mirror"; readonly volume: PlacementVolume; readonly server: MachineRef }
  | { readonly kind: "delete_copy"; readonly volume: PlacementVolume; readonly server: MachineRef };

const toDate = (seconds: number | undefined) => (seconds === undefined ? null : new Date(seconds * 1000));
const isRoot = (member: AnsweredMember) => member.view.copy?.kind === "root";
const leaseOf = (member: AnsweredMember) => member.view.lease?.lease ?? 0;

export function observePlacement(members: readonly Member[]): ObservedPlacement {
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const roots = answered.filter(isRoot).sort((a, b) =>
    Number(b.view.copy?.readonly === false) - Number(a.view.copy?.readonly === false) || leaseOf(b) - leaseOf(a));
  const primary = roots[0] ?? null;
  const copies = answered.flatMap((member): ObservedCopy[] => {
    const copy = member.view.copy;
    if (member === primary || copy === null) return [];
    return [{ server: member.machine, kind: copy.kind, newest: toDate(copy.newest?.created_unix_seconds) }];
  });
  return {
    primary: primary?.machine ?? null,
    copies,
    unanswered: members.filter((member) => !member.answered).map((member) => member.machine),
  };
}

/** The copies that refresh, by Server. */
export function appliedMirrors(observed: ObservedPlacement, row: PlacementRow | null): readonly MachineId[] {
  if (row?.mirrors != null) return row.mirrors;
  return observed.copies.filter((copy) => copy.kind === "slot").map((copy) => copy.server.id);
}

export const detachedName = (volume: PlacementVolume) => `${volume.name}-old`;

/** Copies the dashboard does not list as mirrors: the old primary after a move, kept until the user deletes it. */
export function detachedCopies(observed: ObservedPlacement, row: PlacementRow | null): readonly ObservedCopy[] {
  const mirrors = appliedMirrors(observed, row);
  return observed.copies.filter((copy) => copy.kind === "slot" && !mirrors.includes(copy.server.id));
}

/** What the next Deploy aims for: the staged placement, else what is applied. Null when no primary is known. */
export function targetPlacement(observed: ObservedPlacement, row: PlacementRow | null): Placement | null {
  if (row?.staged != null) return row.staged;
  if (observed.primary === null) return null;
  return { primary: observed.primary.id, mirrors: appliedMirrors(observed, row), deletes: [] };
}

export function placementChanges(volume: PlacementVolume, members: readonly Member[], row: PlacementRow | null): PlacementChange[] {
  const staged = row?.staged;
  if (staged == null) return [];
  const observed = observePlacement(members);
  const machine = (id: MachineId): MachineRef =>
    members.find((member) => member.machine.id === id)?.machine ?? { id, name: id };
  const copyOn = (id: MachineId) => observed.copies.find((copy) => copy.server.id === id && copy.kind === "slot");
  const applied = appliedMirrors(observed, row);
  const detached = detachedCopies(observed, row).filter((copy) => !staged.deletes.includes(copy.server.id));
  const clearing = (keep: MachineId) => detached.filter((copy) => copy.server.id !== keep).map((copy) => copy.server);
  const changes: PlacementChange[] = staged.deletes
    .filter((id) => copyOn(id) !== undefined && !applied.includes(id))
    .map((id) => ({ kind: "delete_copy", volume, server: machine(id) }));
  const moving = observed.primary?.id !== staged.primary;
  for (const id of applied) {
    if (!staged.mirrors.includes(id) && !(moving && id === staged.primary)) changes.push({ kind: "remove_mirror", volume, server: machine(id) });
  }
  if (moving) {
    const target = copyOn(staged.primary);
    const how: MoveHow = target !== undefined && observed.primary === null ? "restore" : applied.includes(staged.primary) ? "promote" : "copy";
    const from = observed.primary ?? (how === "restore" ? observed.unanswered[0] ?? null : null);
    changes.push({
      kind: "move",
      volume,
      from,
      to: machine(staged.primary),
      how,
      lostAfter: how === "restore" ? target?.newest ?? null : null,
      clears: how === "copy" ? clearing(staged.primary) : [],
    });
  }
  for (const id of staged.mirrors) {
    if (!applied.includes(id)) changes.push({ kind: "add_mirror", volume, server: machine(id), clears: clearing(id) });
  }
  return changes;
}

const clock = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit" });
export const lossTime = (at: Date) => clock.format(at);

const clearsWords = (volume: PlacementVolume, clears: readonly MachineRef[]) =>
  clears.length === 0 ? "" : ` Deletes ${detachedName(volume)} on ${clears.map((server) => server.name).join(", ")} first.`;

const MOVE_WORDS = {
  copy: (change) => ({
    title: `Move ${change.volume.name} to ${change.to.name}`,
    detail: `Copies ${change.volume.name} to ${change.to.name} first, then a short stop.${clearsWords(change.volume, change.clears)}`,
  }),
  promote: (change) => ({
    title: `Move ${change.volume.name} to ${change.to.name}`,
    detail: `Uses the mirror on ${change.to.name}, then a short stop. ${change.from?.name ?? "The old server"} keeps ${detachedName(change.volume)}.`,
  }),
  restore: (change) => ({
    title: `Use mirror on ${change.to.name}`,
    detail: `${change.from?.name ?? "The primary"} is not answering. ${change.lostAfter === null ? "Recent changes lost." : `Changes after ${lossTime(change.lostAfter)} lost.`}`,
  }),
} satisfies Record<MoveHow, (change: Extract<PlacementChange, { kind: "move" }>) => PlacementWords>;

export type PlacementWords = { readonly title: string; readonly detail: string };

export function placementWords(change: PlacementChange): PlacementWords {
  switch (change.kind) {
    case "move":
      return MOVE_WORDS[change.how](change);
    case "add_mirror":
      return {
        title: `Add mirror on ${change.server.name}`,
        detail: `Copies ${change.volume.name} to ${change.server.name}. Refreshes on every deploy.${clearsWords(change.volume, change.clears)}`,
      };
    case "remove_mirror":
      return { title: `Remove mirror on ${change.server.name}`, detail: `Deletes ${copyName(change.volume.name, change.server.name)}.` };
    case "delete_copy":
      return { title: `Delete ${detachedName(change.volume)}`, detail: `Deletes the old copy on ${change.server.name}.` };
  }
}

const deleteSlot = (server: MachineRef): VolumeRunInput => ({ kind: "delete_mirror", args: { slot: server.name, confirmed_name: null } });

/** Each change's runs; deletions free the one copy slot before a move or a mirror fills it. */
const STEPS = {
  delete_copy: { order: 0, runs: (change) => [deleteSlot(change.server)] },
  remove_mirror: { order: 1, runs: (change) => [deleteSlot(change.server)] },
  move: {
    order: 2,
    runs: (change) => [
      ...change.clears.map(deleteSlot),
      change.how === "restore" ? { kind: "restore", args: { from: change.to.name } } : { kind: "move", args: { to: change.to.name } },
    ],
  },
  add_mirror: { order: 3, runs: (change) => [...change.clears.map(deleteSlot), { kind: "mirror", args: { to: change.server.name } }] },
} satisfies { [K in PlacementChange["kind"]]: { order: number; runs: (change: Extract<PlacementChange, { kind: K }>) => VolumeRunInput[] } };

function runsOf(change: PlacementChange): VolumeRunInput[] {
  switch (change.kind) {
    case "delete_copy":
      return STEPS.delete_copy.runs(change);
    case "remove_mirror":
      return STEPS.remove_mirror.runs(change);
    case "move":
      return STEPS.move.runs(change);
    case "add_mirror":
      return STEPS.add_mirror.runs(change);
  }
}

/** The runs a Deploy makes, in order. Each run observes the copies again, so a Deploy plans one step at a time. */
export function placementSteps(changes: readonly PlacementChange[]): VolumeRunInput[] {
  return [...changes].sort((a, b) => STEPS[a.kind].order - STEPS[b.kind].order).flatMap(runsOf);
}

/** The mirrors once `change` has run. A move consumes the mirror it promotes. */
export function mirrorsAfter(mirrors: readonly MachineId[], change: PlacementChange): MachineId[] {
  switch (change.kind) {
    case "move":
      return mirrors.filter((id) => id !== change.to.id);
    case "add_mirror":
      return [...mirrors.filter((id) => id !== change.server.id), change.server.id];
    case "remove_mirror":
      return mirrors.filter((id) => id !== change.server.id);
    case "delete_copy":
      return [...mirrors];
  }
}

/** The primary went silent and a mirror answers: the drawer and the tray offer it. */
export type Failover = { readonly down: MachineRef; readonly mirror: MachineRef; readonly lostAfter: Date | null };

export function failover(observed: ObservedPlacement, row: PlacementRow | null): Failover | null {
  const down = observed.unanswered[0];
  if (observed.primary !== null || down === undefined) return null;
  const mirrors = appliedMirrors(observed, row);
  const mirror = observed.copies.find((copy) => copy.kind === "slot" && mirrors.includes(copy.server.id));
  if (mirror === undefined || row?.staged?.primary === mirror.server.id) return null;
  return { down, mirror: mirror.server, lostAfter: mirror.newest };
}

export const failoverWords = (offer: Failover) =>
  [`${offer.down.name} is not answering`, `Use mirror on ${offer.mirror.name}`, offer.lostAfter === null ? null : `Changes after ${lossTime(offer.lostAfter)} lost`]
    .filter((part) => part !== null)
    .join(" · ");

export type PlacementTargets = { readonly move: readonly MachineRef[]; readonly mirror: readonly MachineRef[] };

/**
 * Servers a move or a mirror may target, by what Cloud's planner accepts once detached copies are gone, since the
 * Deploy deletes those first. `servers` names the Servers that accept Services.
 */
export function placementTargets(
  volume: PlacementVolume,
  members: readonly Member[],
  row: PlacementRow | null,
  servers: readonly MachineId[],
): PlacementTargets {
  const observed = observePlacement(members);
  const detached = new Set(detachedCopies(observed, row).map((copy) => copy.server.id));
  const cleared = members.map((member): Member =>
    member.answered && detached.has(member.machine.id) ? { ...member, view: { ...member.view, copy: null } } : member);
  const accepts = (kind: "move" | "mirror", member: Member) => {
    const planned = planFromCopies({ kind, args: { to: member.machine.name }, volumeName: volume.name, orphan: false }, cleared);
    return planned.ok;
  };
  const candidates = members.filter((member) => servers.includes(member.machine.id));
  const mirrors = appliedMirrors(observed, row);
  return {
    move: candidates.filter((member) => accepts("move", member)).map((member) => member.machine),
    mirror: mirrors.length > 0 ? [] : candidates.filter((member) => accepts("mirror", member)).map((member) => member.machine),
  };
}

const CHANGE_KINDS = { move: "update", add_mirror: "add", remove_mirror: "remove", delete_copy: "remove" } as const satisfies Record<PlacementChange["kind"], ChangeKind>;

/** A Volume's staged placement as one group in Details, beside the Store's changes. Discard drops all of it. */
export function placementGroup(volume: PlacementVolume, changes: readonly PlacementChange[]): ChangeGroup {
  return {
    owner: "placement",
    nodeType: "volume",
    nodeId: volume.id,
    nodeName: volume.name,
    discardPath: volume.id,
    row: null,
    lifecycle: "update",
    changeCount: changes.length,
    restarts: [],
    canDiscard: true,
    rows: changes.map((change, index) => {
      const words = placementWords(change);
      return {
        changeKey: `${volume.id}:placement:${index}`,
        label: words.title,
        name: words.title,
        variable: false,
        kind: CHANGE_KINDS[change.kind],
        path: volume.id,
        row: null,
        currentValue: "",
        newValue: "",
        detail: words.detail,
        canDiscard: changes.length === 1,
      };
    }),
  };
}
