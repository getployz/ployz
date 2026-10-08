import type { LeaseRecord, MachineId, VolumeCopy } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { participantsOf, type PlanInput, planFromCopies, type Role, roleOf } from "#/modules/volume-run/plan";
import type { Member } from "#/modules/volume-run/volume-run";

const snapshot = { name: "ployz-1", guid: "11", created_unix_seconds: 1_790_000_000 };
const lease = (cycle: LeaseRecord["cycle"]): LeaseRecord => ({ lease: 4, pos: { seq: 4, round: 0, sub: 5 }, cycle });

const COPIES = {
  writer: { kind: "root", writer: { phase: "idle" }, readonly: false, newest: snapshot },
  switching: { kind: "root", writer: { phase: "frozen", guid: "11" }, readonly: true, newest: snapshot },
  handed: { kind: "root", writer: { phase: "handed", guid: "11" }, readonly: true, newest: snapshot },
  mirror: { kind: "slot", mirror: { phase: "idle" }, readonly: true, newest: snapshot, resume_token: null },
  final: { kind: "slot", mirror: { phase: "final", guid: "11" }, readonly: true, newest: snapshot, resume_token: null },
  stale: { kind: "slot", mirror: { phase: "handed_in", guid: "11" }, readonly: true, newest: snapshot, resume_token: null },
  promoting: { kind: "slot", mirror: { phase: "promoting" }, readonly: true, newest: snapshot, resume_token: null },
  stopping: { kind: "root", writer: { phase: "stopping" }, readonly: false, newest: snapshot },
  thawing: { kind: "root", writer: { phase: "thawing" }, readonly: true, newest: snapshot },
  behind: { kind: "slot", mirror: { phase: "idle" }, readonly: true, newest: { ...snapshot, guid: "10" }, resume_token: null },
  promoted: { kind: "root", writer: { phase: "idle" }, readonly: true, newest: snapshot },
} satisfies Record<string, VolumeCopy>;

type Copy = keyof typeof COPIES | "empty" | "unanswered";

function member(server: string, copy: Copy, options: { pool?: boolean; cycle?: LeaseRecord["cycle"] } = {}): Member {
  const machine = { id: server.padEnd(32, "0") as MachineId, name: server };
  if (copy === "unanswered") return { machine, answered: false };
  const view = { copy: copy === "empty" ? null : (COPIES[copy] ?? null), lease: options.cycle === undefined ? null : lease(options.cycle) };
  return { machine, address: "fdcc::1", answered: true, pool: options.pool ?? copy !== "empty", view };
}

const mirror = (to: string): PlanInput => ({ kind: "mirror", args: { to }, volumeName: "data", orphan: false });
const sync = (full = false): PlanInput => ({ kind: "sync", args: { full }, volumeName: "data", orphan: false });
const remove = (slot: string | null, confirmed: string | null = null): PlanInput =>
  ({ kind: "delete_mirror", args: { slot, confirmed_name: confirmed }, volumeName: "data", orphan: false });
const move = (to: string): PlanInput => ({ kind: "move", args: { to }, volumeName: "data", orphan: false });
const release: PlanInput = { kind: "release", args: {}, volumeName: "data", orphan: false };
const restore = (from: string): PlanInput => ({ kind: "restore", args: { from }, volumeName: "data", orphan: false });
const orphan: PlanInput = { kind: "delete_mirror", args: { slot: null, confirmed_name: null }, volumeName: "ns_vol-1", orphan: true };

function outcome(input: PlanInput, members: Member[]) {
  const planned = planFromCopies(input, members);
  if (!planned.ok) return `refuse ${planned.refusal.code}`;
  const { phase } = planned;
  switch (phase.kind) {
    case "mirror":
      return `mirror ${phase.writer.machine.name}->${phase.target.machine.name}${phase.declare ? " declare" : ""}`;
    case "sync":
      return `sync ${phase.writer.machine.name}->${phase.mirror.machine.name}${phase.full ? " full" : ""}`;
    case "delete_mirror":
      return `delete ${phase.destroy.map((slot) => slot.machine.name).join(",")} forget ${phase.forget?.machine.name ?? "none"} lease ${phase.forgetLease.map((slot) => slot.machine.name).join(",")}`;
    case "move":
      return `move ${phase.writer.machine.name}->${phase.target.machine.name} at ${phase.start}${phase.guid === null ? "" : ` ${phase.guid}`}${phase.declare ? " declare" : ""}`;
    case "release":
      return `release ${phase.source.machine.name}${phase.thaw ? " thaw" : ""} mirror ${phase.mirror?.machine.name ?? "none"}`;
    case "restore":
      return `restore ${phase.from.machine.name}${phase.lostAfter === null ? "" : ` ${phase.lostAfter.guid}`}`;
  }
}

describe("roleOf", () => {
  it.each<[Copy, Role]>([
    ["writer", "writer"],
    ["switching", "switching"],
    ["handed", "handed"],
    ["mirror", "mirror"],
    ["final", "mirror"],
    ["stale", "stale"],
    ["promoting", "stale"],
    ["empty", "empty"],
    ["unanswered", "unanswered"],
  ])("reads %s as %s", (copy, role) => {
    expect(roleOf(member("a", copy))).toBe(role);
  });
});

describe("plan_from_copies_table", () => {
  it.each<[string, PlanInput, Member[], string]>([
    ["mirror onto an empty Server declares it", mirror("b"), [member("a", "writer"), member("b", "empty", { pool: true })], "mirror a->b declare"],
    ["mirror onto its mirror runs rounds only", mirror("b"), [member("a", "writer"), member("b", "mirror")], "mirror a->b"],
    ["mirror onto a final mirror runs rounds only", mirror("b"), [member("a", "writer"), member("b", "final")], "mirror a->b"],
    ["mirror onto a Server with no Pool", mirror("b"), [member("a", "writer"), member("b", "empty", { pool: false })], "refuse no_pool"],
    ["mirror onto the writer", mirror("a"), [member("a", "writer"), member("b", "empty")], "refuse invalid"],
    ["mirror onto a Server not in the cluster", mirror("z"), [member("a", "writer"), member("b", "empty")], "refuse invalid"],
    ["mirror with a mirror elsewhere", mirror("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["mirror with a Server unanswered", mirror("b"), [member("a", "writer"), member("b", "empty"), member("c", "unanswered")], "refuse unanswered"],
    ["mirror with two writers", mirror("c"), [member("a", "writer"), member("b", "writer"), member("c", "empty")], "refuse two_writers"],
    ["mirror with a writer and a handed root", mirror("c"), [member("a", "writer"), member("b", "handed"), member("c", "empty")], "refuse two_writers"],
    ["mirror while the writer switches", mirror("b"), [member("a", "switching"), member("b", "empty")], "refuse volume_switching"],
    ["mirror after the writer handed off", mirror("b"), [member("a", "handed"), member("b", "empty")], "refuse volume_switching"],
    ["mirror with no writer", mirror("b"), [member("a", "empty"), member("b", "empty")], "refuse no_writer"],
    ["mirror with only a mirror", mirror("b"), [member("a", "mirror"), member("b", "empty")], "refuse no_writer"],
    ["mirror with a stale slot (handed in)", mirror("c"), [member("a", "writer"), member("b", "stale"), member("c", "empty")], "refuse stale_slot"],
    ["mirror with a stale slot (promoting)", mirror("c"), [member("a", "writer"), member("b", "promoting"), member("c", "empty")], "refuse stale_slot"],
    ["sync onto its mirror", sync(), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "sync a->b"],
    ["sync --full onto its mirror", sync(true), [member("a", "writer"), member("b", "final")], "sync a->b full"],
    ["sync with no mirror", sync(), [member("a", "writer"), member("b", "empty")], "refuse no_mirror"],
    ["sync with two mirrors", sync(), [member("a", "writer"), member("b", "mirror"), member("c", "mirror")], "refuse second_mirror"],
    ["sync with a stale slot", sync(), [member("a", "writer"), member("b", "stale")], "refuse stale_slot"],
    ["sync with no writer", sync(), [member("b", "mirror")], "refuse no_writer"],
    ["sync with a Server unanswered", sync(), [member("a", "writer"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],
    ["delete one mirror forgets on the writer", remove("b"), [member("a", "writer"), member("b", "mirror")], "delete b forget a lease b"],
    ["delete a stale slot", remove("b"), [member("a", "writer"), member("b", "stale")], "delete b forget a lease b"],
    ["delete every slot", remove(null), [member("a", "writer"), member("b", "mirror"), member("c", "stale")], "delete b,c forget a lease b,c"],
    ["delete a Server with no slot", remove("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse no_mirror"],
    ["delete the only copy unconfirmed", remove("b"), [member("a", "empty"), member("b", "mirror")], "refuse confirm_required"],
    ["delete the only copy confirmed", remove("b", "data"), [member("a", "empty"), member("b", "mirror")], "delete b forget none lease b"],
    ["delete the only copy confirmed wrongly", remove("b", "other"), [member("b", "mirror")], "refuse confirm_required"],
    ["delete while the writer switches", remove("b"), [member("a", "switching"), member("b", "mirror")], "refuse volume_switching"],
    ["delete while the writer's lease cycle is open", remove("b"), [member("a", "writer", { cycle: "open" }), member("b", "mirror")], "refuse volume_switching"],
    ["delete after a closed cycle", remove("b"), [member("a", "writer", { cycle: "closed" }), member("b", "mirror")], "delete b forget a lease b"],
    ["delete with a Server unanswered", remove("b"), [member("a", "writer"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],
    ["orphan delete destroys every slot and forgets every lease record", orphan, [member("b", "mirror"), member("c", "stale"), member("d", "empty", { pool: true }), member("e", "empty")], "delete b,c forget none lease b,c,d"],
    ["orphan delete ignores an unanswered Server", orphan, [member("b", "mirror"), member("c", "unanswered")], "delete b forget none lease b"],
    ["orphan delete of a name with a writer", orphan, [member("a", "writer"), member("b", "mirror")], "refuse invalid"],
    ["orphan delete with no slot left", orphan, [member("b", "empty")], "refuse no_mirror"],
    ["move onto an empty Server declares it", move("b"), [member("a", "writer"), member("b", "empty", { pool: true })], "move a->b at rounds declare"],
    ["move onto its mirror runs rounds", move("b"), [member("a", "writer"), member("b", "mirror")], "move a->b at rounds"],
    ["move onto the writer", move("a"), [member("a", "writer"), member("b", "mirror")], "refuse invalid"],
    ["move with a mirror elsewhere", move("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["move onto a Server with no Pool", move("b"), [member("a", "writer"), member("b", "empty", { pool: false })], "refuse no_pool"],
    ["move with a stale slot", move("c"), [member("a", "writer"), member("b", "stale"), member("c", "empty")], "refuse stale_slot"],
    ["move with a Server unanswered", move("b"), [member("a", "handed"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],
    ["move with no writer", move("b"), [member("a", "empty"), member("b", "mirror")], "refuse no_writer"],
    ["move again once frozen and sent hands over", move("b"), [member("a", "switching"), member("b", "mirror")], "move a->b at handover 11"],
    ["move again once frozen and sent to a final mirror hands over", move("b"), [member("a", "switching"), member("b", "final")], "move a->b at handover 11"],
    ["move again once frozen and not sent undoes", move("b"), [member("a", "switching"), member("b", "behind")], "move a->b at undo 11"],
    ["move again while stopping undoes", move("b"), [member("a", "stopping"), member("b", "mirror")], "move a->b at undo"],
    ["move again while thawing undoes", move("b"), [member("a", "thawing"), member("b", "mirror")], "move a->b at undo"],
    ["move again once frozen, toward another Server, undoes", move("c"), [member("a", "switching"), member("b", "mirror"), member("c", "empty")], "move a->c at undo 11"],
    ["move again after handover accepts", move("b"), [member("a", "handed"), member("b", "mirror")], "move a->b at accept 11"],
    ["move again after handover onto a final mirror accepts", move("b"), [member("a", "handed"), member("b", "final")], "move a->b at accept 11"],
    ["move again after accept promotes, never accepts again", move("b"), [member("a", "handed"), member("b", "stale")], "move a->b at promote 11"],
    ["move again mid-promote promotes", move("b"), [member("a", "handed"), member("b", "promoting")], "move a->b at promote 11"],
    ["move again on a read-only root promotes", move("b"), [member("a", "handed"), member("b", "promoted", { cycle: "open" })], "move a->b at promote 11"],
    ["move again after promote starts", move("b"), [member("a", "handed"), member("b", "writer", { cycle: "open" })], "move a->b at start 11"],
    ["move again after start closes", move("b"), [member("a", "handed"), member("b", "writer", { cycle: "closed" })], "move a->b at close 11"],
    ["move after handover toward another Server", move("c"), [member("a", "handed"), member("b", "stale"), member("c", "empty")], "refuse volume_switching"],
    ["move after handover with the snapshot nowhere", move("b"), [member("a", "handed"), member("b", "behind")], "refuse invalid"],
    ["mirror after handover onto the target", mirror("b"), [member("a", "handed"), member("b", "writer", { cycle: "open" })], "refuse two_writers"],
    ["release an idle writer with a final mirror", release, [member("a", "writer"), member("b", "final")], "release a mirror b"],
    ["release an idle writer with a mirror", release, [member("a", "writer"), member("b", "mirror")], "release a mirror none"],
    ["release a frozen writer clears the final on its mirror", release, [member("a", "switching"), member("b", "mirror")], "release a thaw mirror b"],
    ["release a frozen writer not sent yet", release, [member("a", "switching"), member("b", "behind")], "release a thaw mirror none"],
    ["release a stopping writer", release, [member("a", "stopping"), member("b", "mirror")], "release a thaw mirror none"],
    ["release a thawing writer", release, [member("a", "thawing")], "release a thaw mirror none"],
    ["release after handover", release, [member("a", "handed"), member("b", "stale")], "refuse volume_switching"],
    ["release with a Server unanswered", release, [member("a", "switching"), member("b", "unanswered")], "refuse unanswered"],
    ["release with no writer", release, [member("b", "mirror")], "refuse no_writer"],
    ["restore the only mirror", restore("b"), [member("a", "empty"), member("b", "mirror")], "restore b 11"],
    ["restore a final mirror", restore("b"), [member("b", "final")], "restore b 11"],
    ["restore a stale slot", restore("b"), [member("a", "empty"), member("b", "stale")], "restore b 11"],
    ["restore a slot mid-promote", restore("b"), [member("b", "promoting")], "restore b 11"],
    ["restore a handed root", restore("a"), [member("a", "handed"), member("b", "empty")], "restore a"],
    ["restore a frozen root", restore("a"), [member("a", "switching")], "restore a"],
    ["restore a read-only root", restore("a"), [member("a", "promoted")], "restore a"],
    ["restore the writable writer", restore("a"), [member("a", "writer"), member("b", "empty")], "refuse invalid"],
    ["restore while another Server holds the writer", restore("b"), [member("a", "writer"), member("b", "mirror")], "refuse another_copy"],
    ["restore while another Server holds a mirror", restore("a"), [member("a", "handed"), member("b", "stale")], "refuse another_copy"],
    ["restore while another Server holds a frozen root", restore("b"), [member("a", "switching"), member("b", "mirror")], "refuse another_copy"],
    ["restore from a Server with no copy", restore("b"), [member("a", "empty"), member("b", "empty")], "refuse no_copy"],
    ["restore from a Server not in the cluster", restore("z"), [member("b", "mirror")], "refuse invalid"],
    ["restore with a Server unanswered", restore("b"), [member("a", "unanswered"), member("b", "mirror")], "refuse unanswered"],
  ])("%s", (_, input, members, expected) => {
    expect(outcome(input, members)).toBe(expected);
  });

  it("names the copy and the confirmation in the only-copy refusal", () => {
    expect(planFromCopies(remove("fsn-2"), [member("fsn-2", "mirror")])).toEqual({
      ok: false,
      refusal: { code: "confirm_required", message: "data-fsn-2 is data's only copy; volume mirror rm data-fsn-2 --confirm data" },
    });
  });

  it("names the Server a handed Volume continues on", () => {
    expect(planFromCopies(release, [member("fsn-1", "handed"), member("fsn-2", "stale")])).toEqual({
      ok: false,
      refusal: { code: "volume_switching", message: "data is handed to fsn-2; volume move data --to fsn-2 again continues from there" },
    });
  });

  it("names every other copy Restore would leave behind", () => {
    expect(planFromCopies(restore("fsn-2"), [member("fsn-1", "handed"), member("fsn-2", "stale"), member("fsn-3", "mirror")])).toEqual({
      ok: false,
      refusal: {
        code: "another_copy",
        message: "data also has copies on fsn-1, fsn-3; Restore needs data-fsn-2 to be the only copy, so remove the others with server rm or volume mirror rm first",
      },
    });
  });

  it("names the Server without managed storage", () => {
    expect(planFromCopies(mirror("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "empty", { pool: false })])).toEqual({
      ok: false,
      refusal: { code: "no_pool", message: "fsn-2 has no managed volume storage yet" },
    });
  });
});

describe("participantsOf", () => {
  const named = (input: PlanInput, members: Member[]) => {
    const planned = planFromCopies(input, members);
    if (!planned.ok) throw new Error(planned.refusal.message);
    return participantsOf(planned.phase, members).map((id) => id.replace(/0+$/, "")).sort();
  };

  it.each<[string, PlanInput, Member[], string[]]>([
    ["a Move onto an empty Server names its source and target", move("b"), [member("a", "writer"), member("b", "empty", { pool: true }), member("d", "empty")], ["a", "b"]],
    ["a Sync names both copies and not the empty Servers", sync(), [member("a", "writer"), member("c", "mirror"), member("d", "empty", { pool: true })], ["a", "c"]],
    ["a Mirror names the empty Server it builds on", mirror("b"), [member("a", "writer"), member("b", "empty", { pool: true })], ["a", "b"]],
    ["a Restore names the copy it makes the writer", restore("b"), [member("a", "empty"), member("b", "mirror")], ["b"]],
  ])("%s", (_name, input, members, expected) => {
    expect(named(input, members)).toEqual(expected);
  });
});
