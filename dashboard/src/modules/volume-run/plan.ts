import type { ExpectedCopy, MachineName, Snapshot, SnapshotGuid, VolumeCopy } from "@ployz/sdk";
import { type AnsweredMember, copyName, type Member, type ReleasePlan, type VolumeRunArgs, type VolumeRunInput } from "#/modules/volume-run/volume-run";

/** `cutting` is a read-only root: a cut began on it and has not renamed it to a slot yet. */
export type Role = "writer" | "cutting" | "mirror" | "empty" | "unanswered";

export type RefusalCode =
  | "unanswered"
  | "server_lacks"
  | "no_writer"
  | "two_writers"
  | "volume_switching"
  | "second_mirror"
  | "invalid"
  | "no_mirror"
  | "no_copy"
  | "another_copy"
  | "confirm_required";

/**
 * Where a Move continues from the copies it finds. `rounds` and `cut` start on A's root; `final` pulls the cut's
 * snapshot to B; `start` makes B the writer and removes A's kept Container. From `start` on a Move only goes forward.
 */
export type MoveStart = "rounds" | "cut" | "final" | "start";

export type Refusal = { readonly code: RefusalCode; readonly message: string };

/**
 * What a Release finds in flight: a cut that stopped on a read-only root, a writer that may still keep the moved-off
 * Container, or only slots, where one of them is the cut source that keeps its stopped Container.
 */
export type ReleaseFrom =
  | { readonly kind: "cut"; readonly root: AnsweredMember }
  | { readonly kind: "writer"; readonly writer: AnsweredMember; readonly behind: readonly AnsweredMember[] }
  | { readonly kind: "slots"; readonly slots: readonly AnsweredMember[]; readonly plan: ReleasePlan | undefined };

export type Planned =
  | { readonly ok: true; readonly phase: { readonly kind: "mirror"; readonly writer: AnsweredMember; readonly target: AnsweredMember } }
  | { readonly ok: true; readonly phase: { readonly kind: "sync"; readonly writer: AnsweredMember; readonly mirror: AnsweredMember; readonly full: boolean } }
  | { readonly ok: true; readonly phase: { readonly kind: "delete_mirror"; readonly destroy: readonly AnsweredMember[]; readonly forget: AnsweredMember | null } }
  | { readonly ok: true; readonly phase: { readonly kind: "move"; readonly writer: AnsweredMember; readonly target: AnsweredMember; readonly start: MoveStart } }
  | { readonly ok: true; readonly phase: { readonly kind: "landed"; readonly target: AnsweredMember; readonly behind: readonly AnsweredMember[] } }
  | { readonly ok: true; readonly phase: { readonly kind: "release"; readonly from: ReleaseFrom } }
  | { readonly ok: true; readonly phase: { readonly kind: "restore"; readonly from: AnsweredMember; readonly lostAfter: Snapshot | null } }
  | { readonly ok: true; readonly phase: { readonly kind: "demote"; readonly old: AnsweredMember; readonly writer: AnsweredMember } }
  | { readonly ok: true; readonly phase: { readonly kind: "seed"; readonly source: AnsweredMember; readonly args: VolumeRunArgs["seed"] } }
  | { readonly ok: false; readonly refusal: Refusal };

export type PlanInput = VolumeRunInput & { readonly volumeName: string; readonly orphan: boolean };

export function roleOf(member: Member): Role {
  if (!member.answered) return "unanswered";
  const copy = member.view.copy;
  if (copy === null) return "empty";
  if (copy.kind === "slot") return "mirror";
  return copy.readonly ? "cutting" : "writer";
}

const isUnadoptedSeed = (member: Member) =>
  member.answered && member.view.copy?.kind === "root" && member.view.copy.seed;

export const epochOf = (member: AnsweredMember) => (member.view.copy?.kind === "root" ? member.view.copy.epoch : 0);

export const newestGuid = (copy: VolumeCopy | null): SnapshotGuid | null => copy?.newest?.guid ?? null;

/**
 * What a SetVolumeCopy expects of a copy the run saw: a slot that holds no snapshot yet counts as none. A slot is named
 * by its id too, since a slot pulled again after a Forget holds the same snapshot guids.
 */
export function expectOf(copy: VolumeCopy | null): ExpectedCopy {
  if (copy === null) return { kind: "none" };
  if (copy.kind === "root") return { kind: "writer", epoch: copy.epoch };
  return copy.newest === null ? { kind: "none" } : { kind: "mirror", newest: copy.newest.guid, slot: copy.slot };
}

const SNAPSHOT_EPOCH = /^[wf]-(\d+)/;

/** The highest epoch any answered copy shows, in its root or in the run that named its newest snapshot. */
export function observedEpoch(members: readonly Member[]): number {
  return Math.max(0, ...members.flatMap((member) => {
    if (!member.answered) return [];
    const named = SNAPSHOT_EPOCH.exec(member.view.copy?.newest?.name ?? "");
    return [epochOf(member), named === null ? 0 : Number(named[1])];
  }));
}

const isDestination = ({ to, machine }: VolumeRunArgs["move"], member: Member) =>
  machine === undefined ? member.machine.name === to : member.machine.id === machine;

const refuse = (code: RefusalCode, message: string): Planned => ({ ok: false, refusal: { code, message } });

/** Pure: Inngest re-runs it outside any step on every replay, so the same members must plan the same run. */
export function planFromCopies(input: PlanInput, members: readonly Member[]): Planned {
  const name = input.volumeName;
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const withRole = (...roles: Role[]) => answered.filter((member) => roles.includes(roleOf(member)));
  const named = (member: Member) => copyName(name, member.machine.name);
  const roots = withRole("writer", "cutting");
  const cutting = withRole("cutting");
  const mirrors = withRole("mirror");
  const silent = members.filter((member): member is Exclude<Member, AnsweredMember> => !member.answered);
  const unanswered = silent.find((member) => member.lack !== undefined) ?? silent[0];
  const unknown = (member: Exclude<Member, AnsweredMember>) => member.lack === undefined
    ? refuse("unanswered", `${member.machine.name} did not answer; ${name}'s copies are unknown`)
    : refuse("server_lacks", member.lack);
  const unfinishedCuts = () => {
    // As `oldCopy` reads roots, the cut at the highest epoch began last, so its writes are the ones to keep.
    const keptFirst = (a: AnsweredMember, b: AnsweredMember) => epochOf(b) - epochOf(a) || Number(holds(b)) - Number(holds(a)) || byServer(a, b);
    const keep = cutting.reduce((best, each) => (keptFirst(each, best) < 0 ? each : best));
    const { remove, step } = releaseOrMoveOntoSlot(name, keep, mirrors);
    const older = cutting.filter((member) => member !== keep);
    return refuse("volume_switching", `${name} has unfinished cuts on ${servers(cutting)}, so release cannot pick a writer; ${serverRm([...older, ...remove])}${step}`);
  };

  if (input.kind === "delete_mirror") {
    if (input.orphan) {
      const writers = roots.filter((member) => !isUnadoptedSeed(member));
      if (writers.length > 0) return refuse("invalid", `${name} still has a writer on ${writers.map(named).join(", ")}`);
      const destroy = [...roots, ...mirrors];
      if (destroy.length === 0) return refuse("no_mirror", `${name} has no mirror`);
      return { ok: true, phase: { kind: "delete_mirror", destroy, forget: null } };
    }
    if (unanswered !== undefined) return unknown(unanswered);
    const writers = withRole("writer");
    if (writers.length > 1) return refuse("two_writers", `${name} has two writers, ${writers.map(named).join(" and ")}`);
    if (cutting.length > 0) return writers.length > 0 ? midRun(name, mirrors) : cutting.length > 1 ? unfinishedCuts() : midRun(name, mirrors, cutting[0]);
    const { slot, confirmed_name } = input.args;
    const destroy = slot === null ? mirrors : mirrors.filter((member) => member.machine.name === slot);
    if (destroy.length === 0) return refuse("no_mirror", slot === null ? `${name} has no mirror` : `${name} has no mirror on ${slot}`);
    if (roots.length === 0 && confirmed_name !== name) {
      const only = destroy.map(named).join(", ");
      return refuse("confirm_required", `${only} is ${name}'s only copy; volume mirror rm ${only} --confirm ${name}`);
    }
    const [writer] = roots;
    return { ok: true, phase: { kind: "delete_mirror", destroy, forget: roots.length === 1 && writer !== undefined ? writer : null } };
  }

  if (unanswered !== undefined) return unknown(unanswered);
  if (input.kind === "restore") return planRestore(name, input.args, answered);
  if (roots.length > 1) {
    const old = oldCopy(roots);
    if (old === null && withRole("writer").length < 2) return unfinishedCuts();
    if (old === null) return refuse("two_writers", `${name} has two writers, ${roots.map(named).join(" and ")}`);
    return { ok: true, phase: { kind: "demote", ...old } };
  }
  const [root] = roots;

  if (input.kind === "seed") {
    if (root === undefined || roleOf(root) !== "writer") return root === undefined ? refuse("no_writer", `${name} has no primary to restore from`) : midRun(name, mirrors, root);
    return { ok: true, phase: { kind: "seed", source: root, args: input.args } };
  }

  if (input.kind === "release") {
    if (root === undefined) {
      if (mirrors.length === 0) return refuse("no_writer", `${name} has no copy to release`);
      return { ok: true, phase: { kind: "release", from: { kind: "slots", slots: mirrors, plan: input.args.plan } } };
    }
    if (roleOf(root) === "cutting") return newestGuid(root.view.copy) === null ? stoppedEarly(name, root, mirrors) : { ok: true, phase: { kind: "release", from: { kind: "cut", root } } };
    const at = newestGuid(root.view.copy);
    const behind = at === null ? [] : mirrors.filter((member) => newestGuid(member.view.copy) === at);
    return { ok: true, phase: { kind: "release", from: { kind: "writer", writer: root, behind } } };
  }

  if (input.kind === "move") return planMove(name, input.args, answered, root ?? null);

  if (root === undefined) {
    if (mirrors.length > 0) return refuse("no_writer", `${name} has no writer; ployz volume release ${name} ends a move cut short`);
    return refuse("no_writer", `${name} has no writer; deploy ${name} before mirroring it`);
  }
  if (roleOf(root) === "cutting") return midRun(name, mirrors, root);

  if (input.kind === "sync") {
    const [mirror, second] = mirrors;
    if (mirror === undefined) return refuse("no_mirror", `${name} has no mirror; volume mirror ${name} --to <server>`);
    if (second !== undefined) return refuse("second_mirror", `${name} already has a mirror, ${named(mirror)}`);
    return { ok: true, phase: { kind: "sync", writer: root, mirror, full: input.args.full } };
  }

  const { to } = input.args;
  if (isDestination(input.args, root)) return { ok: false, refusal: alreadyOn(name, to) };
  const elsewhere = mirrors.find((member) => !isDestination(input.args, member));
  if (elsewhere !== undefined) return refuse("second_mirror", `${name} already has a mirror, ${named(elsewhere)}`);
  const target = answered.find((member) => isDestination(input.args, member));
  if (target === undefined) return refuse("invalid", `${to} is not a Server of this cluster`);
  return { ok: true, phase: { kind: "mirror", writer: root, target } };
}

const holds = (member: AnsweredMember) => newestGuid(member.view.copy) !== null;
const byServer = (a: AnsweredMember, b: AnsweredMember) => a.machine.name.localeCompare(b.machine.name);
const servers = (each: readonly AnsweredMember[]) => each.map((member) => member.machine.name).join(" and ");
const serverRm = (remove: readonly AnsweredMember[]) => remove.map(({ machine }) => `ployz server rm ${machine.name}, then `).join("");

/** Release resumes a cut from its newest snapshot, so a cut with none moves again, onto the slot a root may keep. */
function releaseOrMoveOntoSlot(name: string, kept: AnsweredMember, slots: readonly AnsweredMember[]) {
  if (holds(kept)) return { remove: [], step: `ployz volume release ${name}` };
  const [onto, ...extra] = [...slots].sort(byServer);
  return { remove: extra, step: `ployz volume move ${name} --to ${onto?.machine.name ?? "<another server>"}` };
}

const stoppedEarly = (name: string, cut: AnsweredMember, slots: readonly AnsweredMember[]): Planned => {
  const { remove, step } = releaseOrMoveOntoSlot(name, cut, slots);
  return refuse("no_copy", `${name} on ${cut.machine.name} stopped before its first snapshot; ${serverRm(remove)}${step}`);
};
const midRun = (name: string, slots: readonly AnsweredMember[], lone?: AnsweredMember): Planned => lone !== undefined && !holds(lone)
  ? stoppedEarly(name, lone, slots)
  : refuse("volume_switching", `${name} is mid-run; ployz volume release ${name} stops it`);

/**
 * A Move is read from the copies alone. A is the root, or with no root the one slot besides B; G is A's newest once
 * A is a slot. The Move needs B's Writer while B's slot holds G, and is done but for A's Container once B is the writer
 * at G. Then A is whichever slot at G keeps the stopped Container, which only the run can see.
 */
function planMove(name: string, args: VolumeRunArgs["move"], answered: readonly AnsweredMember[], root: AnsweredMember | null): Planned {
  const named = (member: Member) => copyName(name, member.machine.name);
  const { to } = args;
  const target = answered.find((member) => isDestination(args, member));
  if (target === undefined) return refuse("invalid", `${to} is not a Server of this cluster`);
  const others = answered.filter((member) => member !== target && member.view.copy?.kind === "slot");
  const move = (writer: AnsweredMember, start: MoveStart): Planned => ({ ok: true, phase: { kind: "move", writer, target, start } });

  if (root === target) {
    if (roleOf(root) === "cutting") return midRun(name, others, root);
    const at = newestGuid(root.view.copy);
    const behind = at === null ? [] : others.filter((member) => newestGuid(member.view.copy) === at);
    if (behind.length === 0) return { ok: false, refusal: alreadyOn(name, to) };
    return { ok: true, phase: { kind: "landed", target, behind } };
  }
  const elsewhere = others.find((member) => member !== root);
  if (root !== null) {
    if (elsewhere !== undefined) return refuse("second_mirror", `${name} already has a mirror, ${named(elsewhere)}`);
    return move(root, roleOf(root) === "cutting" ? "cut" : "rounds");
  }

  const [source, second] = others;
  if (source === undefined) return refuse("no_writer", `${name} has no writer; deploy ${name} before moving it`);
  if (second !== undefined) return refuse("second_mirror", `${name} has copies on ${others.map(named).join(" and ")}; ployz volume release ${name} first`);
  const at = newestGuid(source.view.copy);
  if (at === null) return refuse("no_writer", `${name} has no writer; deploy ${name} before moving it`);
  return move(source, newestGuid(target.view.copy) === at ? "start" : "final");
}

export const alreadyOn = (name: string, server: string): Refusal => ({ code: "invalid", message: `${name}'s writer is already on ${server}` });

/** Restore makes the one copy left the writer, so any copy elsewhere, answering or not, would become a second writer. */
function planRestore(name: string, { from, machine }: VolumeRunArgs["restore"], answered: readonly AnsweredMember[]): Planned {
  const source = answered.find((member) => (machine === undefined ? member.machine.name === from : member.machine.id === machine));
  if (source === undefined) return refuse("invalid", `${from} is not a Server of this cluster`);
  const copy = source.view.copy;
  if (copy === null) return refuse("no_copy", `${from} holds no copy of ${name}`);
  if (roleOf(source) === "writer") return { ok: false, refusal: alreadyOn(name, from) };
  if (copy.newest === null) return refuse("no_copy", `${copyName(name, from)} holds no snapshot to restore`);
  const others = answered.filter((member) => member !== source && member.view.copy !== null);
  if (others.length > 0) {
    const servers = others.map((member) => member.machine.name).join(", ");
    return refuse(
      "another_copy",
      `${name} also has copies on ${servers}; Restore needs ${copyName(name, from)} to be the only copy, so remove the others with server rm or volume mirror rm first`,
    );
  }
  // A slot holds the data as of its newest snapshot; a root holds every write, so restoring it loses none.
  return { ok: true, phase: { kind: "restore", from: source, lostAfter: copy.kind === "slot" ? copy.newest : null } };
}

export function restoreSource(members: readonly Member[]): MachineName | undefined {
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  if (answered.length < members.length) return undefined;
  const stopped = answered.flatMap((member) => {
    const newest = member.view.copy?.newest ?? null;
    return newest === null || roleOf(member) === "writer" ? [] : [{ name: member.machine.name, newest }];
  });
  const second = Math.max(...stopped.map(({ newest }) => newest.created_unix_seconds));
  const top = stopped.filter(({ newest }) => newest.created_unix_seconds === second);
  return new Set(top.map(({ newest }) => newest.guid)).size === 1 ? top[0]?.name : undefined;
}

export const restoreLine = (volumeName: string, holder: MachineName | undefined) => holder === undefined
  ? ". No other server's copy is known to hold its newest data."
  : `. Restore from ${holder}'s copy: ployz volume restore ${volumeName} --from ${holder}`;

export const writerRemovedMessage = (volumeName: string, removed: string, holder: MachineName | undefined) =>
  `${volumeName}'s writer was on ${removed} (removed)` + restoreLine(volumeName, holder);

export type ForwardRelease =
  | {
      readonly kind: "ready";
      readonly target: AnsweredMember;
      readonly copy: VolumeCopy & { readonly newest: Snapshot };
      readonly removeOn: readonly Member["machine"][];
    }
  | { readonly kind: "waiting"; readonly servers: readonly Member["machine"][]; readonly removeOn: readonly Member["machine"][] }
  | { readonly kind: "blocked"; readonly message: string };

/**
 * How a release finishes a committed Move on its new Server `to`. Every Server must answer first: while B is a slot,
 * starting it reads A's stopped Container. The rest are where the moved-off Container is removed.
 */
export function planForwardRelease(volumeName: string, to: Member["machine"], members: readonly Member[]): ForwardRelease {
  const target = members.find((member) => member.machine.id === to.id);
  if (target === undefined) return { kind: "blocked", message: writerRemovedMessage(volumeName, to.name, restoreSource(members)) };
  for (const member of members) if (!member.answered && member.lack !== undefined) return { kind: "blocked", message: member.lack };
  const removeOn = members.filter((member) => member !== target).map((member) => member.machine);
  const silent = members.filter((member) => !member.answered);
  if (silent.length > 0) return { kind: "waiting", servers: silent.map((member) => member.machine), removeOn };
  const copy = target.answered ? target.view.copy : null;
  if (!target.answered || copy === null || copy.newest === null) {
    return { kind: "blocked", message: `${volumeName}'s Move committed, but ${to.name} holds no copy to start` + restoreLine(volumeName, restoreSource(members)) };
  }
  const writer = members.find((member) => member !== target && roleOf(member) === "writer");
  if (writer !== undefined) {
    const holder = writer.machine.name;
    return {
      kind: "blocked",
      message: `${copyName(volumeName, holder)} is still a writer copy, so ${volumeName} can't finish on ${to.name}; ployz volume restore ${volumeName} --from ${holder} keeps ${holder}'s data`,
    };
  }
  const cuts = unfinishedCutsBefore(volumeName, target, members);
  if (cuts !== null) return { kind: "blocked", message: cuts };
  return { kind: "ready", target, copy: { ...copy, newest: copy.newest }, removeOn };
}

/** A release finishes an owed Move on `target` only once no cut is left outside it, so every cut goes first. */
function unfinishedCutsBefore(volumeName: string, target: AnsweredMember, members: readonly Member[]): string | null {
  const cuts = members.filter((member): member is AnsweredMember => member !== target && roleOf(member) === "cutting").sort(byServer);
  if (cuts.length === 0 || !holds(target)) return null;
  const { remove, step } = releaseOrMoveOntoSlot(volumeName, target, []);
  const each = cuts.length === 1 ? "an unfinished cut" : "unfinished cuts";
  return `${volumeName} has ${each} on ${servers(cuts)}, so release cannot finish it on ${target.machine.name}; ${serverRm([...cuts, ...remove])}${step}`;
}

/**
 * A Server removed without a reset keeps its root; once Restore made a newer one elsewhere, the root with the lower
 * epoch is old. Only a writable root can be the writer, so a read-only one is old beside any writable one. Writable
 * roots whose epochs tie leave no way to tell, so neither is old.
 */
export function oldCopy(roots: readonly AnsweredMember[]): { readonly old: AnsweredMember; readonly writer: AnsweredMember } | null {
  const writable = (root: AnsweredMember) => roleOf(root) === "writer";
  const above = (a: AnsweredMember, b: AnsweredMember) => Number(writable(a)) - Number(writable(b)) || epochOf(a) - epochOf(b);
  const [writer, next] = [...roots].sort((a, b) => above(b, a));
  const old = [...roots].sort((a, b) => above(a, b))[0];
  if (writer === undefined || next === undefined || old === undefined || !writable(writer) || above(writer, next) === 0) return null;
  return { old, writer };
}
