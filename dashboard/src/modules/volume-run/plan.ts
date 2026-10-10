import type { SnapshotGuid } from "@ployz/sdk";
import { type AnsweredMember, copyName, type Member, type VolumeRunInput } from "#/modules/volume-run/volume-run";

export type Role = "writer" | "switching" | "handed" | "mirror" | "stale" | "empty" | "unanswered";

export type RefusalCode =
  | "unanswered"
  | "no_writer"
  | "two_writers"
  | "volume_switching"
  | "stale_slot"
  | "second_mirror"
  | "invalid"
  | "no_pool"
  | "no_mirror"
  | "confirm_required";

/** Where a Move continues from the copies it finds; every step after `handover` is past the point of no return. */
export type MoveStart = "rounds" | "handover" | "accept" | "promote" | "start" | "close" | "undo";

export type Refusal = { readonly code: RefusalCode; readonly message: string };

export type Planned =
  | { readonly ok: true; readonly phase: { readonly kind: "mirror"; readonly writer: AnsweredMember; readonly target: AnsweredMember; readonly declare: boolean } }
  | { readonly ok: true; readonly phase: { readonly kind: "sync"; readonly writer: AnsweredMember; readonly mirror: AnsweredMember; readonly full: boolean } }
  | { readonly ok: true; readonly phase: { readonly kind: "delete_mirror"; readonly destroy: readonly AnsweredMember[]; readonly forget: AnsweredMember | null; readonly forgetLease: readonly AnsweredMember[] } }
  | {
      readonly ok: true;
      readonly phase: {
        readonly kind: "move";
        readonly writer: AnsweredMember;
        readonly target: AnsweredMember;
        readonly start: MoveStart;
        readonly guid: SnapshotGuid | null;
        readonly declare: boolean;
      };
    }
  | { readonly ok: true; readonly phase: { readonly kind: "release"; readonly source: AnsweredMember; readonly mirror: AnsweredMember | null; readonly thaw: boolean } }
  | { readonly ok: false; readonly refusal: Refusal };

type Phase = Extract<Planned, { ok: true }>["phase"];
type MovePhase = Extract<Phase, { kind: "move" }>;

/** Undo clears the target's final only when the target holds a slot that this or an earlier attempt filled. */
export const undoClearsTarget = (phase: MovePhase) => phase.start === "rounds" || phase.target.view.copy?.kind === "slot";

/** The Machines the planned phase sends a verb to; only their records may raise the next lease. */
export function participantsOf(phase: Phase): readonly AnsweredMember[] {
  switch (phase.kind) {
    case "mirror":
      return [phase.writer, phase.target];
    case "move":
      switch (phase.start) {
        case "close":
          return [phase.writer];
        case "undo":
          return undoClearsTarget(phase) ? [phase.writer, phase.target] : [phase.writer];
        case "rounds":
        case "handover":
        case "accept":
        case "promote":
        case "start":
          return [phase.writer, phase.target];
      }
    case "sync":
      return [phase.writer, phase.mirror];
    case "release":
      return [...(phase.thaw ? [phase.source] : []), ...(phase.mirror === null ? [] : [phase.mirror])];
    case "delete_mirror":
      return [...phase.destroy, ...(phase.forget === null ? [] : [phase.forget]), ...phase.forgetLease];
  }
}

export type PlanInput = VolumeRunInput & { readonly volumeName: string; readonly orphan: boolean };

export function roleOf(member: Member): Role {
  if (!member.answered) return "unanswered";
  const copy = member.view.copy;
  if (copy === null) return "empty";
  if (copy.kind === "root") {
    switch (copy.writer.phase) {
      case "idle":
        return "writer";
      case "handed":
        return "handed";
      case "stopping":
      case "frozen":
      case "thawing":
        return "switching";
    }
  }
  switch (copy.mirror.phase) {
    case "idle":
    case "final":
      return "mirror";
    case "handed_in":
    case "promoting":
      return "stale";
  }
}

/** The snapshot guid a root hands over: the final snapshot it froze or handed. */
function frozenGuid(member: AnsweredMember): SnapshotGuid | null {
  const copy = member.view.copy;
  if (copy?.kind !== "root") return null;
  return copy.writer.phase === "frozen" || copy.writer.phase === "handed" ? copy.writer.guid : null;
}

function holds(member: AnsweredMember, guid: SnapshotGuid): boolean {
  const copy = member.view.copy;
  if (copy === null) return false;
  if (copy.kind === "slot" && copy.mirror.phase === "handed_in") return copy.mirror.guid === guid;
  return copy.newest?.guid === guid;
}

/** Past `handover` a Move only goes forward, so the target's copy names the next step and Accept never re-runs on it. */
function handedStart(target: AnsweredMember): MoveStart {
  const copy = target.view.copy;
  if (copy?.kind === "slot") return copy.mirror.phase === "idle" || copy.mirror.phase === "final" ? "accept" : "promote";
  if (copy?.readonly !== false) return "promote";
  return target.view.lease?.cycle === "open" ? "start" : "close";
}

const isFinal = (member: AnsweredMember) => member.view.copy?.kind === "slot" && member.view.copy.mirror.phase === "final";

const refuse = (code: RefusalCode, message: string): Planned => ({ ok: false, refusal: { code, message } });

/** Pure: Inngest re-runs it outside any step on every replay, so the same members must plan the same run. */
export function planFromCopies(input: PlanInput, members: readonly Member[]): Planned {
  const name = input.volumeName;
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const withRole = (...roles: Role[]) => answered.filter((member) => roles.includes(roleOf(member)));
  const named = (member: Member) => copyName(name, member.machine.name);
  const roots = withRole("writer", "switching", "handed");
  const switching = withRole("switching", "handed");
  const writer = withRole("writer")[0];
  const mirrors = withRole("mirror");
  const stale = withRole("stale");
  const unanswered = members.find((member) => !member.answered);
  const midRun = () => refuse("volume_switching", `${name} is mid-run; wait or volume release ${name}`);

  if (input.kind === "delete_mirror") {
    const slots = [...mirrors, ...stale];
    if (input.orphan) {
      if (roots.length > 0) return refuse("invalid", `${name} still has a writer on ${roots.map(named).join(", ")}`);
      if (slots.length === 0) return refuse("no_mirror", `${name} has no mirror`);
      return { ok: true, phase: { kind: "delete_mirror", destroy: slots, forget: null, forgetLease: answered.filter((member) => slots.includes(member) || member.view.lease !== null) } };
    }
    if (unanswered !== undefined) return refuse("unanswered", `${unanswered.machine.name} did not answer; ${name}'s copies are unknown`);
    if (switching.length > 0 || writer?.view.lease?.cycle === "open") return midRun();
    const { slot, confirmed_name } = input.args;
    const destroy = slot === null ? slots : slots.filter((member) => member.machine.name === slot);
    if (destroy.length === 0) return refuse("no_mirror", slot === null ? `${name} has no mirror` : `${name} has no mirror on ${slot}`);
    if (roots.length === 0 && confirmed_name !== name) {
      const only = destroy.map(named).join(", ");
      return refuse("confirm_required", `${only} is ${name}'s only copy; volume mirror rm ${only} --confirm ${name}`);
    }
    return { ok: true, phase: { kind: "delete_mirror", destroy, forget: writer ?? null, forgetLease: destroy } };
  }

  if (unanswered !== undefined) return refuse("unanswered", `${unanswered.machine.name} did not answer; ${name}'s copies are unknown`);
  const [source] = switching;
  if (source !== undefined && switching.length === 1) {
    const guid = frozenGuid(source);
    const holder = guid === null ? undefined : answered.find((member) => member !== source && holds(member, guid));
    const handedTo = (server: string) => `${name} is handed to ${server}; volume move ${name} --to ${server} again continues from there`;
    if (input.kind === "release") {
      if (roleOf(source) === "handed") return refuse("volume_switching", holder === undefined ? `${name} is handed off` : handedTo(holder.machine.name));
      return { ok: true, phase: { kind: "release", source, mirror: holder ?? mirrors.find(isFinal) ?? null, thaw: true } };
    }
    if (input.kind === "move") {
      const { to } = input.args;
      const target = answered.find((member) => member.machine.name === to);
      if (target === undefined) return refuse("invalid", `${to} is not a Server of this cluster`);
      if (roleOf(source) === "handed") {
        if (holder === undefined) return refuse("invalid", `no Server holds the snapshot ${named(source)} handed over`);
        if (holder !== target) return refuse("volume_switching", handedTo(holder.machine.name));
        return { ok: true, phase: { kind: "move", writer: source, target, start: handedStart(target), guid, declare: false } };
      }
      const start = holder === target && target.view.copy?.kind === "slot" ? "handover" : "undo";
      return { ok: true, phase: { kind: "move", writer: source, target, start, guid, declare: false } };
    }
  }
  if (roots.length > 1) return refuse("two_writers", `${name} has two writers, ${roots.map(named).join(" and ")}`);
  if (switching.length > 0) return midRun();
  if (writer === undefined) return refuse("no_writer", `${name} has no writer; deploy ${name} before mirroring it`);
  const firstStale = stale[0];
  if (firstStale !== undefined) {
    return refuse("stale_slot", `${named(firstStale)} holds a stale copy; volume mirror rm ${named(firstStale)} first`);
  }
  if (input.kind === "release") return { ok: true, phase: { kind: "release", source: writer, mirror: mirrors.find(isFinal) ?? null, thaw: false } };

  if (input.kind === "sync") {
    const [mirror, second] = mirrors;
    if (mirror === undefined) return refuse("no_mirror", `${name} has no mirror; volume mirror ${name} --to <server>`);
    if (second !== undefined) return refuse("second_mirror", `${name} already has a mirror, ${named(mirror)}`);
    return { ok: true, phase: { kind: "sync", writer, mirror, full: input.args.full } };
  }

  const { to } = input.args;
  if (writer.machine.name === to) return refuse("invalid", `${name}'s writer is already on ${to}`);
  const elsewhere = mirrors.find((member) => member.machine.name !== to);
  if (elsewhere !== undefined) return refuse("second_mirror", `${name} already has a mirror, ${named(elsewhere)}`);
  const target = answered.find((member) => member.machine.name === to);
  if (target === undefined) return refuse("invalid", `${to} is not a Server of this cluster`);
  if (!target.pool) return refuse("no_pool", `${to} has no managed volume storage yet`);
  const declare = roleOf(target) !== "mirror";
  if (input.kind === "move") return { ok: true, phase: { kind: "move", writer, target, start: "rounds", guid: null, declare } };
  return { ok: true, phase: { kind: "mirror", writer, target, declare } };
}
