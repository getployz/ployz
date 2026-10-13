import type { ChangeKind, MachineId, MachineName } from "@ployz/sdk";
import type { DeletionItem } from "#/components/deletion-dialog";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { STALE_AFTER_MS } from "#/modules/volume-copies/volume-copies";
import { epochOf, planFromCopies } from "#/modules/volume-run/plan";
import { runWords } from "#/modules/volume-run/volume-copies";
import type { RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { type AnsweredMember, type Member, parseDockerVolumeName, type VolumeRunInput, type VolumeRunView } from "#/modules/volume-run/volume-run";

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

/** One snapshot a mirror holds, which a new Volume can be restored from. */
export type RestorePoint = { readonly guid: string; readonly name: string; readonly at: Date; readonly server: MachineRef };

/** A new Volume's data comes from one restore point of `sourceVolumeId`, copied on the source's primary. */
export type Seed = {
  readonly sourceVolumeId: string;
  readonly sourceName: string;
  /** The new Volume's Docker name, which the copy lands under. */
  readonly dockerVolume: string;
  /** `at` in Unix seconds, as the jsonb row keeps it. */
  readonly point: { readonly guid: string; readonly name: string; readonly at: number };
};

/** Where the user wants the Volume. `deletes` names detached copies to delete. */
export type Placement = {
  readonly primary: MachineId;
  readonly mirrors: readonly MachineId[];
  readonly deletes: readonly MachineId[];
  readonly seed?: Seed;
};

/** The row's `staged` as stored: a failed Deploy marks it unreached in place, and the next one clears the mark. */
export type StoredPlacement = Placement & { readonly unreached?: true };

/**
 * The dashboard's record of a Volume's placement. `mirrors` are the copies that refresh; a copy on any other Server is
 * a detached volume. Null `mirrors` means no record yet, so every copy counts as a mirror, as the CLI made them.
 */
export type PlacementRow = {
  readonly volumeId: string;
  readonly environmentId: string;
  readonly mirrors: readonly MachineId[] | null;
  readonly staged: Placement | null;
  /** A failed Deploy gave `staged` back before it got to it. */
  readonly unreached: boolean;
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
    /** When the copy it moves to last refreshed. */
    readonly refreshedAt: Date | null;
    /** Detached copies the move deletes first, since a Volume holds one copy besides its primary for now. */
    readonly clears: readonly MachineRef[];
  }
  | { readonly kind: "add_mirror"; readonly volume: PlacementVolume; readonly server: MachineRef; readonly clears: readonly MachineRef[] }
  | { readonly kind: "remove_mirror"; readonly volume: PlacementVolume; readonly server: MachineRef }
  | { readonly kind: "delete_copy"; readonly volume: PlacementVolume; readonly server: MachineRef }
  | {
    readonly kind: "seed";
    readonly volume: PlacementVolume;
    readonly source: { readonly id: string; readonly name: string };
    readonly to: MachineRef;
    readonly dockerVolume: string;
    readonly point: Seed["point"];
  };

const toDate = (seconds: number | undefined) => (seconds === undefined ? null : new Date(seconds * 1000));
const isRoot = (member: AnsweredMember) => member.view.copy?.kind === "root";

export function observePlacement(members: readonly Member[]): ObservedPlacement {
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const roots = answered.filter(isRoot).sort((a, b) =>
    Number(b.view.copy?.readonly === false) - Number(a.view.copy?.readonly === false) || epochOf(b) - epochOf(a));
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

export function oldCopies(observed: ObservedPlacement, row: PlacementRow | null): readonly ObservedCopy[] {
  const target = targetPlacement(observed, row);
  return detachedCopies(observed, row).filter((copy) => copy.server.id !== target?.primary && !target?.mirrors.includes(copy.server.id));
}

/**
 * What the next Deploy aims for: the staged placement, else what is applied. With no primary answering, the silent
 * Server is presumed to hold it. Null when no Server holds the Volume.
 */
export function targetPlacement(observed: ObservedPlacement, row: PlacementRow | null): Placement | null {
  if (row?.staged != null) return row.staged;
  const primary = observed.primary ?? observed.unanswered[0] ?? null;
  if (primary === null) return null;
  return { primary: primary.id, mirrors: appliedMirrors(observed, row), deletes: [] };
}

export type PlacementEdit =
  | { readonly kind: "move"; readonly to: MachineId }
  | { readonly kind: "add_mirror" | "remove_mirror" | "delete_copy"; readonly server: MachineId };

/** One drawer edit. A move to a mirror promotes it; any other move leaves Mirrors as they are. */
export function editPlacement(target: Placement, edit: PlacementEdit): Placement {
  switch (edit.kind) {
    case "move":
      return { ...target, primary: edit.to, mirrors: target.mirrors.filter((id) => id !== edit.to) };
    case "add_mirror":
      return { ...target, mirrors: [...target.mirrors, edit.server] };
    case "remove_mirror":
      return { ...target, mirrors: target.mirrors.filter((id) => id !== edit.server) };
    case "delete_copy":
      return { ...target, deletes: [...target.deletes, edit.server] };
  }
}

export function placementChanges(volume: PlacementVolume, members: readonly Member[], row: PlacementRow | null): PlacementChange[] {
  const staged = row?.staged;
  if (staged == null) return [];
  const observed = observePlacement(members);
  const machine = (id: MachineId): MachineRef =>
    members.find((member) => member.machine.id === id)?.machine ?? { id, name: id };
  // Once any root of the new Volume answers, the copy landed; a second seed would make a second root.
  if (staged.seed !== undefined) {
    const { seed } = staged;
    if (observed.primary !== null) return [];
    return [{
      kind: "seed",
      volume,
      source: { id: seed.sourceVolumeId, name: seed.sourceName },
      to: machine(staged.primary),
      dockerVolume: seed.dockerVolume,
      point: seed.point,
    }];
  }
  const copyOn = (id: MachineId) => observed.copies.find((copy) => copy.server.id === id && copy.kind === "slot");
  const applied = appliedMirrors(observed, row);
  const detached = detachedCopies(observed, row).filter((copy) => !staged.deletes.includes(copy.server.id));
  const clearing = (keep: MachineId) => detached.filter((copy) => copy.server.id !== keep).map((copy) => copy.server);
  const changes: PlacementChange[] = staged.deletes
    .filter((id) => copyOn(id) !== undefined && !applied.includes(id))
    .map((id) => ({ kind: "delete_copy", volume, server: machine(id) }));
  // A move to a mirror is unfinished while that mirror is still listed: once the target writes it holds the root,
  // yet the next Deploy must finish the same move, not take the mirror away.
  const handing = observed.primary?.id === staged.primary && applied.includes(staged.primary);
  // With no primary answering, a silent Server presumed to hold the Volume is no move: the staged one when it is silent,
  // since a failed move returns there, else the first, as targetPlacement presumes.
  const holder = observed.primary ?? observed.unanswered.find((server) => server.id === staged.primary) ?? observed.unanswered[0] ?? null;
  const moving = holder?.id !== staged.primary || handing;
  // A mirror its answering Server no longer holds is already removed: a cancelled Deploy's late run took it.
  const gone = (id: MachineId) => copyOn(id) === undefined && members.some((member) => member.machine.id === id && member.answered);
  for (const id of applied) {
    if (!staged.mirrors.includes(id) && id !== staged.primary && !gone(id)) changes.push({ kind: "remove_mirror", volume, server: machine(id) });
  }
  if (moving) {
    const target = copyOn(staged.primary);
    const how: MoveHow = target !== undefined && observed.primary === null ? "restore" : applied.includes(staged.primary) ? "promote" : "copy";
    const from = handing
      ? observed.copies.find((copy) => copy.kind === "slot" && copy.server.id !== staged.primary)?.server ?? null
      : observed.primary ?? (how === "restore" ? observed.unanswered[0] ?? null : null);
    changes.push({
      kind: "move",
      volume,
      from,
      to: machine(staged.primary),
      how,
      lostAfter: how === "restore" ? target?.newest ?? null : null,
      refreshedAt: target?.newest ?? null,
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
  seed: {
    order: 2,
    runs: (change) => [{
      kind: "seed",
      args: { into: { volumeId: change.volume.id, dockerVolume: change.dockerVolume, name: change.volume.name }, guid: change.point.guid, name: change.point.name },
    }],
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
    case "seed":
      return STEPS.seed.runs(change);
  }
}

/** The runs a Deploy makes, in order. Each run observes the copies again, so a Deploy plans one step at a time. */
export function placementSteps(changes: readonly PlacementChange[]): VolumeRunInput[] {
  return [...changes].sort((a, b) => STEPS[a.kind].order - STEPS[b.kind].order).flatMap(runsOf);
}

/**
 * Names a change across a Deploy's wait, so a failure finds the same change among the ones read afresh. A move keeps the
 * Server it left: once that Server stops answering, it is still the one holding the Volume.
 */
export type ChangeKey = { readonly kind: PlacementChange["kind"]; readonly server: MachineId; readonly from?: MachineId };

export const changeKey = (change: PlacementChange): ChangeKey => {
  if (change.kind === "move") return change.from === null ? { kind: "move", server: change.to.id } : { kind: "move", server: change.to.id, from: change.from.id };
  return { kind: change.kind, server: change.kind === "seed" ? change.to.id : change.server.id };
};

const sameChange = (a: ChangeKey, b: ChangeKey) => a.kind === b.kind && a.server === b.server;

/** The next run a Deploy makes, and the mirrors once it ends: a change's mirrors move only with its last run. */
export type PlacementStep = { readonly run: VolumeRunInput; readonly mirrors: readonly MachineId[]; readonly change: ChangeKey };

export function nextPlacementStep(changes: readonly PlacementChange[], mirrors: readonly MachineId[]): PlacementStep | null {
  const [change] = [...changes].sort((a, b) => STEPS[a.kind].order - STEPS[b.kind].order);
  if (change === undefined) return null;
  const [run, ...rest] = runsOf(change);
  if (run === undefined) return null;
  return { run, mirrors: rest.length === 0 ? mirrorsAfter(mirrors, change) : [...mirrors], change: changeKey(change) };
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
    case "seed":
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
    return planned.ok && planned.phase.kind !== "landed";
  };
  const candidates = members.filter((member) => servers.includes(member.machine.id));
  const mirrors = appliedMirrors(observed, row);
  return {
    move: candidates.filter((member) => accepts("move", member)).map((member) => member.machine),
    mirror: mirrors.length > 0 ? [] : candidates.filter((member) => accepts("mirror", member)).map((member) => member.machine),
  };
}

/**
 * Where a Drain of `server` stages the Volume: its primary moves to its mirror, else to the first other Server that
 * accepts Services and that the planner takes. Null when the primary is elsewhere, or nowhere takes it.
 */
export function drainPlacement(
  volume: PlacementVolume, members: readonly Member[], row: PlacementRow | null, accepting: readonly MachineId[], server: MachineId,
): Placement | null {
  const target = targetPlacement(observePlacement(members), row);
  if (target === null || target.primary !== server) return null;
  const to = placementTargets(volume, members, row, accepting.filter((id) => id !== server)).move[0];
  return to === undefined ? null : editPlacement(target, { kind: "move", to: to.id });
}

/** How many Volumes whose primary is on `server` have a move off it staged, by the copies the Runtime Watch reports. */
export function stagedOffServer(server: string, rows: readonly PlacementRow[], copies: readonly RuntimeVolumeCopy[]): number {
  const primaries = new Set(copies.filter((copy) => copy.role === "writer" && copy.machineId === server)
    .flatMap((copy) => parseDockerVolumeName(copy.name)?.volumeId ?? []));
  return rows.filter((row) => primaries.has(row.volumeId) && row.staged !== null && row.staged.primary !== server).length;
}

const CHANGE_KINDS = { move: "update", add_mirror: "add", remove_mirror: "remove", delete_copy: "remove", seed: "add" } as const satisfies Record<PlacementChange["kind"], ChangeKind>;

/** One Server in the Placement form, and what the next Deploy does to it: add, update, remove, or nothing (null). */
export type PlacementItem = {
  readonly server: MachineRef;
  readonly change: ChangeKind | null;
  /** When its copy last refreshed, as its Server reports it. */
  readonly newest: Date | null;
};

/** The Placement form: the Primary, its Mirrors, and the detached copies kept until the user deletes them. */
export type PlacementForm = {
  readonly primary: PlacementItem;
  readonly mirrors: readonly PlacementItem[];
  readonly detached: readonly PlacementItem[];
};

export function placementForm(volume: PlacementVolume, members: readonly Member[], row: PlacementRow | null): PlacementForm | null {
  const observed = observePlacement(members);
  const target = targetPlacement(observed, row);
  if (target === null) return null;
  const machine = (id: MachineId): MachineRef =>
    members.find((member) => member.machine.id === id)?.machine ?? { id, name: id };
  const changes = placementChanges(volume, members, row);
  const onPrimary = changes.find((change) => change.kind === "move" || change.kind === "seed") ?? null;
  const changeOn = (kinds: readonly PlacementChange["kind"][], id: MachineId) =>
    changes.find((change) => kinds.includes(change.kind) && "server" in change && change.server.id === id) ?? null;
  const cleared = new Set(changes.flatMap((change) => change.kind === "move" || change.kind === "add_mirror" ? change.clears.map((server) => server.id) : []));
  const item = (server: MachineRef, change: ChangeKind | null): PlacementItem =>
    ({ server, change, newest: observed.copies.find((copy) => copy.server.id === server.id)?.newest ?? null });
  const kindOf = (change: PlacementChange | null) => change && CHANGE_KINDS[change.kind];
  const mirrors = [...new Set([...appliedMirrors(observed, row), ...target.mirrors])].filter((id) => id !== target.primary)
    .map((id) => item(machine(id), kindOf(changeOn(["add_mirror", "remove_mirror"], id))));
  const left = onPrimary?.kind === "move" && onPrimary.how !== "restore" && onPrimary.from !== null ? [item(onPrimary.from, "add")] : [];
  const detached = oldCopies(observed, row)
    .map((copy) => item(copy.server, changeOn(["delete_copy"], copy.server.id) || cleared.has(copy.server.id) ? "remove" : null));
  return { primary: item(machine(target.primary), kindOf(onPrimary)), mirrors, detached: [...left, ...detached] };
}

/** A mirror's address among the Environment's resources: its Volume and its Server. */
export const mirrorResourceId = (volumeId: string, server: MachineId) => `${volumeId}~${server}`;

export function parseMirrorResourceId(resourceId: string): { readonly volumeId: string; readonly server: MachineId } | null {
  const [volumeId, server, ...rest] = resourceId.split("~");
  if (!volumeId || !server || rest.length > 0) return null;
  // SAFETY: the address carries a Server id the canvas wrote; a stale one finds no mirror.
  return { volumeId, server: server as MachineId };
}

/**
 * The Volumes of `environmentId` worth asking the Servers about for the canvas: one the dashboard lists a mirror or a
 * staged placement for, or one with a mirror the Runtime Watch reports, as the CLI makes them. The rest have no mirror node.
 */
export function mirroredVolumes(
  volumeIds: readonly string[], rows: readonly PlacementRow[], environmentId: string, copies: readonly RuntimeVolumeCopy[],
): string[] {
  const placed = new Set(rows.filter((row) => row.environmentId === environmentId && (row.staged !== null || (row.mirrors?.length ?? 0) > 0))
    .map((row) => row.volumeId));
  const slots = new Set(copies.filter((copy) => copy.role === "slot").flatMap((copy) => parseDockerVolumeName(copy.name)?.volumeId ?? []));
  return volumeIds.filter((id) => placed.has(id) || slots.has(id));
}

export function undoPlacement(volume: PlacementVolume, members: readonly Member[], row: PlacementRow, change: PlacementChange): Placement | null {
  const staged = row.staged;
  if (staged === null) return null;
  const applied = appliedMirrors(observePlacement(members), row);
  const next = ((): Placement | null => {
    switch (change.kind) {
      case "move":
        return change.from === null ? null : {
          ...staged,
          primary: change.from.id,
          mirrors: applied.includes(change.to.id) ? [...staged.mirrors.filter((id) => id !== change.to.id), change.to.id] : staged.mirrors,
        };
      case "add_mirror":
        return { ...staged, mirrors: staged.mirrors.filter((id) => id !== change.server.id) };
      case "remove_mirror":
        return { ...staged, mirrors: [...staged.mirrors, change.server.id] };
      case "delete_copy":
        return { ...staged, deletes: staged.deletes.filter((id) => id !== change.server.id) };
      case "seed":
        return null;
    }
  })();
  return next === null || placementChanges(volume, members, { ...row, staged: next }).length === 0 ? null : next;
}

/**
 * What stays staged once a Deploy's run for the change `key` names failed: the change goes, and the rest waits for the
 * next Deploy. A change the Servers no longer show landed despite the failure: deletes whose copy is gone go, the rest
 * stays, and the row drops once nothing is left to change. A move the Servers handed over stays whole, since only
 * finishing it brings the Volume back. One whose primary isn't answering goes
 * back to the Server the move left, else the silent Server presumed to hold the Volume, or drops the row when there is none.
 */
export function dropChange(volume: PlacementVolume, members: readonly Member[], row: PlacementRow, key: ChangeKey): Placement | null {
  const change = placementChanges(volume, members, row).find((one) => sameChange(changeKey(one), key));
  if (change === undefined) {
    if (row.staged === null) return null;
    const observed = observePlacement(members);
    const held = (id: MachineId) => observed.copies.some((copy) => copy.server.id === id) || observed.unanswered.some((server) => server.id === id);
    const landed = { ...row.staged, deletes: row.staged.deletes.filter(held) };
    return placementChanges(volume, members, { ...row, staged: landed }).length === 0 ? null : landed;
  }
  if (change.kind === "move") {
    const observed = observePlacement(members);
    if (observed.primary?.id === row.staged?.primary) return row.staged;
    const left = observed.unanswered.find((server) => server.id === key.from);
    const from = observed.primary ?? left ?? observed.unanswered[0] ?? null;
    if (from === null || from.id === change.to.id) return null;
    return undoPlacement(volume, members, row, { ...change, from });
  }
  return undoPlacement(volume, members, row, change);
}

export function changeSteps(change: PlacementChange, now: Date): readonly string[] {
  const clears = "clears" in change && change.clears.length > 0 ? ["Delete"] : [];
  switch (change.kind) {
    case "move": {
      if (change.how === "restore") return ["Stop", "Start"];
      const behind = change.how === "promote" && change.refreshedAt !== null && now.getTime() - change.refreshedAt.getTime() > STALE_AFTER_MS;
      return [...clears, ...change.how === "copy" ? ["Seed"] : [], ...behind ? ["Catch up"] : [], "Final sync", "Stop", "Start"];
    }
    case "add_mirror":
      return [...clears, "Seed"];
    case "remove_mirror":
    case "delete_copy":
      return ["Delete"];
    case "seed":
      return ["Save current", "Stop", "Swap", "Start"];
  }
}

/**
 * One thing a Deploy of staged changes takes: the copy of `volume` on `server`, or a restore's dropped writes, the
 * source's after `after` when a restore point bounds them. A restore drops both the unanswered primary's copy and its
 * writes.
 */
export type PlacementLost = { readonly volume: PlacementVolume; readonly server: MachineRef | null; readonly writes: boolean; readonly after: Date | null };

export function placementLosses(changes: readonly PlacementChange[]): PlacementLost[] {
  const copy = (volume: PlacementVolume, server: MachineRef): PlacementLost => ({ volume, server, writes: false, after: null });
  return changes.flatMap((change) => {
    const cleared = "clears" in change ? change.clears.map((server) => copy(change.volume, server)) : [];
    switch (change.kind) {
      case "move":
        return change.how === "restore" ? [...cleared, { volume: change.volume, server: change.from, writes: true, after: change.lostAfter }] : cleared;
      case "add_mirror":
        return cleared;
      case "remove_mirror":
      case "delete_copy":
        return [copy(change.volume, change.server)];
      case "seed":
        return [{ volume: change.source, server: null, writes: true, after: new Date(change.point.at * 1000) }];
    }
  });
}

type VolumeLost = { name: string; deletes: { machine_id: MachineId }[]; loss: "data" | "writes"; after?: number };

/**
 * Each Volume that loses something, as a `confirmation_required` refusal names it: the Servers losing a copy, and for a
 * restore dropping writes, `writes` and any restore point in seconds, so a refused Deploy words its confirm as the local
 * check does.
 */
export function lossByVolume(lost: readonly PlacementLost[]): VolumeLost[] {
  return [...new Set(lost.map(({ volume }) => volume.name))].map((name) => {
    const of = lost.filter(({ volume }) => volume.name === name);
    const deletes = of.flatMap(({ server }) => server === null ? [] : [{ machine_id: server.id }]);
    const writes = of.filter((entry) => entry.writes);
    if (writes.length === 0) return { name, deletes, loss: "data" };
    // The earliest cutoff loses the most; an unknown one may be earlier still.
    const after = writes.reduce<Date | null>((earliest, { after: cutoff }) => cutoff === null || earliest === null ? null : cutoff < earliest ? cutoff : earliest,
      writes[0]?.after ?? null);
    return after === null ? { name, deletes, loss: "writes" } : { name, deletes, loss: "writes", after: after.getTime() / 1000 };
  });
}

/** Each Volume a Deploy loses, as its confirm lists it, whether Cloud refused it or the canvas checked first. */
export function volumeLossItems(
  volumes: readonly { name: string; deletes: readonly { machine_id: string }[]; loss?: "data" | "writes"; after?: number }[],
  machineName: (id: string) => string | undefined,
): DeletionItem[] {
  return volumes.map((volume) => ({
    kind: "volume",
    name: volume.name,
    detail: volume.deletes.flatMap((held) => machineName(held.machine_id) ?? []).join(", ")
      || (volume.after !== undefined ? `changes after ${lossTime(new Date(volume.after * 1000))}` : volume.loss === "writes" ? "recent changes" : undefined),
  }));
}

const LOSS_MARK = "#loss-";

/**
 * The Store diff version a Deploy carries, bound to the loss its confirm showed. Names alone would let consent to one
 * loss pass for another staged on the same Volume meanwhile, and staging a copy change doesn't move the Store's version.
 * The version is the one field every client carries back verbatim.
 */
export function lossVersion(version: string, lost: readonly PlacementLost[]) {
  if (lost.length === 0) return version;
  const shown = JSON.stringify(lossByVolume(lost)
    .map((volume) => ({ ...volume, deletes: volume.deletes.map(({ machine_id }) => machine_id).sort() }))
    .sort((a, b) => a.name.localeCompare(b.name)));
  let hash = 0x811c9dc5;
  for (let index = 0; index < shown.length; index++) hash = Math.imul(hash ^ shown.charCodeAt(index), 0x01000193);
  return `${version}${LOSS_MARK}${(hash >>> 0).toString(36)}`;
}

/** The Store's own version inside one `lossVersion` bound. */
export const storeVersion = (version: string) => version.split(LOSS_MARK)[0] ?? version;

/**
 * What a Deploy of staged changes takes, for its typed confirm: what goes, whether a restore drops writes, the name of
 * each Volume that loses something, which the Deploy accepts as it accepts a Volume the Store deletes, and the version
 * binding that acceptance to this loss.
 */
export type PlacementLoss = {
  readonly items: readonly DeletionItem[];
  readonly losesWrites: boolean;
  readonly accept: readonly string[];
  readonly version: (version: string) => string;
};

export function placementLoss(changes: readonly PlacementChange[]): PlacementLoss {
  const lost = placementLosses(changes);
  const volumes = lossByVolume(lost);
  const servers = lost.flatMap(({ server }) => server ?? []);
  return {
    items: volumeLossItems(volumes, (id) => servers.find((server) => server.id === id)?.name),
    losesWrites: volumes.some(({ loss }) => loss === "writes"),
    accept: [...new Set(lost.map(({ volume }) => volume.name))],
    version: (version) => lossVersion(version, lost),
  };
}

function detailsRow(volume: PlacementVolume, change: PlacementChange) {
  const clears = "clears" in change && change.clears.length > 0 ? `Deletes ${detachedName(volume)} first` : undefined;
  switch (change.kind) {
    case "move":
      if (change.how !== "restore") return { name: "Primary", currentValue: change.from?.name ?? "", newValue: change.to.name, detail: clears };
      return {
        name: "Primary", currentValue: change.from?.name ?? "", newValue: change.to.name,
        loss: change.lostAfter === null ? "Recent changes lost" : `Changes after ${lossTime(change.lostAfter)} lost`,
      };
    case "add_mirror":
      return { name: "Mirror", currentValue: "", newValue: change.server.name, detail: clears };
    case "remove_mirror":
      return { name: "Mirror", currentValue: change.server.name, newValue: "" };
    case "delete_copy":
      return { name: "Detached copy", currentValue: change.server.name, newValue: "" };
    case "seed": {
      const at = lossTime(new Date(change.point.at * 1000));
      return {
        name: "From", currentValue: "", newValue: `${change.source.name} · ${at}`,
        loss: `Changes after ${at} are not in ${volume.name}`,
        detail: "Volume data, not database point-in-time.",
      };
    }
  }
}

export function placementGroup(volume: PlacementVolume, changes: readonly PlacementChange[], now: Date, unreached = false): ChangeGroup {
  return {
    owner: "placement",
    nodeType: "volume",
    nodeId: volume.id,
    nodeName: volume.name,
    discardPath: volume.id,
    row: null,
    lifecycle: changes.some((change) => change.kind === "seed") ? "create" : "update",
    changeCount: changes.length,
    restarts: [],
    canDiscard: true,
    unreached,
    rows: changes.map((change, index) => {
      const words = detailsRow(volume, change);
      return {
        changeKey: `${volume.id}:placement:${index}`,
        label: words.name,
        ...words,
        steps: changeSteps(change, now),
        variable: false,
        kind: CHANGE_KINDS[change.kind],
        path: String(index),
        row: null,
        canDiscard: true,
      };
    }),
  };
}

/** The snapshots the Volume's mirrors hold, newest first: what a new Volume can be restored from. */
export function restorePoints(members: readonly Member[]): RestorePoint[] {
  const points = new Map<string, RestorePoint>();
  for (const member of members) {
    if (!member.answered) continue;
    const copy = member.view.copy;
    if (copy?.kind !== "slot" || copy.newest === null || points.has(copy.newest.guid)) continue;
    const { guid, name, created_unix_seconds } = copy.newest;
    points.set(guid, { guid, name, at: new Date(created_unix_seconds * 1000), server: member.machine });
  }
  return [...points.values()].sort((a, b) => b.at.getTime() - a.at.getTime());
}

/** What Deploy `deploymentId` is doing to storage, from the run under way on a Volume it took: "pg-data · Moving to web-2". */
export function storageStep(deploymentId: string, rows: readonly PlacementRow[], runs: readonly VolumeRunView[]): string | null {
  const taken = new Set(rows.filter((row) => row.deploymentId === deploymentId).map((row) => row.volumeId));
  const run = runs.find((candidate) => taken.has(candidate.volume_id));
  return run === undefined ? null : [run.volume_name, runWords(run), run.message].filter((part) => part !== null).join(" · ");
}

/**
 * Details shows a restored Volume once. The Store lists it as a created Volume and its placement row says where the
 * data comes from, so both go into one group. Discard takes the whole group through the placement, which removes
 * the Volume and puts the mount back as one write.
 */
export function mergeRestoreGroups(store: readonly ChangeGroup[], placement: readonly ChangeGroup[]): ChangeGroup[] {
  const restored = new Map(placement.filter((group) => group.lifecycle === "create").map((group) => [group.nodeId, group]));
  const merged = store.map((group) => {
    const seed = restored.get(group.nodeId);
    if (seed === undefined) return group;
    restored.delete(group.nodeId);
    return {
      ...seed,
      rows: [...seed.rows, ...group.rows.map((row) => ({ ...row, canDiscard: false }))],
      changeCount: seed.changeCount + group.changeCount,
      restarts: group.restarts,
    };
  });
  return [...merged, ...placement.filter((group) => group.lifecycle !== "create" || restored.has(group.nodeId))];
}
