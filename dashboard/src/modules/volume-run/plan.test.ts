import type { LeaseRecord, MachineId, VolumeCopy } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { type PlanInput, planFromCopies, type Role, roleOf } from "#/modules/volume-run/plan";
import type { Member } from "#/modules/volume-run/volume-run";

const snapshot = { name: "ployz-1", guid: 11, created_unix_seconds: 1_790_000_000 };
const lease = (cycle: LeaseRecord["cycle"]): LeaseRecord => ({ lease: 4, pos: { seq: 4, round: 0, sub: 5 }, cycle });

const COPIES = {
  writer: { kind: "root", writer: { phase: "idle" }, readonly: false, newest: snapshot },
  switching: { kind: "root", writer: { phase: "frozen", guid: 11 }, readonly: true, newest: snapshot },
  handed: { kind: "root", writer: { phase: "handed", guid: 11 }, readonly: true, newest: snapshot },
  mirror: { kind: "slot", mirror: { phase: "idle" }, readonly: true, newest: snapshot, resume_token: null },
  final: { kind: "slot", mirror: { phase: "final", guid: 11 }, readonly: true, newest: snapshot, resume_token: null },
  stale: { kind: "slot", mirror: { phase: "handed_in", guid: 11 }, readonly: true, newest: snapshot, resume_token: null },
  promoting: { kind: "slot", mirror: { phase: "promoting" }, readonly: true, newest: snapshot, resume_token: null },
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
const orphan: PlanInput = { kind: "delete_mirror", args: { slot: null, confirmed_name: null }, volumeName: "ns_vol-1", orphan: true };

/** What a plan comes to, in one comparable word: the refusal code, or the Servers each step touches. */
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
      return `delete ${phase.destroy.map((slot) => slot.machine.name).join(",")} forget ${phase.forget?.machine.name ?? "none"}`;
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
    ["delete one mirror forgets on the writer", remove("b"), [member("a", "writer"), member("b", "mirror")], "delete b forget a"],
    ["delete a stale slot", remove("b"), [member("a", "writer"), member("b", "stale")], "delete b forget a"],
    ["delete every slot", remove(null), [member("a", "writer"), member("b", "mirror"), member("c", "stale")], "delete b,c forget a"],
    ["delete a Server with no slot", remove("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse no_mirror"],
    ["delete the only copy unconfirmed", remove("b"), [member("a", "empty"), member("b", "mirror")], "refuse confirm_required"],
    ["delete the only copy confirmed", remove("b", "data"), [member("a", "empty"), member("b", "mirror")], "delete b forget none"],
    ["delete the only copy confirmed wrongly", remove("b", "other"), [member("b", "mirror")], "refuse confirm_required"],
    ["delete while the writer switches", remove("b"), [member("a", "switching"), member("b", "mirror")], "refuse volume_switching"],
    ["delete while the writer's lease cycle is open", remove("b"), [member("a", "writer", { cycle: "open" }), member("b", "mirror")], "refuse volume_switching"],
    ["delete after a closed cycle", remove("b"), [member("a", "writer", { cycle: "closed" }), member("b", "mirror")], "delete b forget a"],
    ["delete with a Server unanswered", remove("b"), [member("a", "writer"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],
    ["orphan delete destroys every slot, forgets nothing", orphan, [member("b", "mirror"), member("c", "stale"), member("d", "empty")], "delete b,c forget none"],
    ["orphan delete ignores an unanswered Server", orphan, [member("b", "mirror"), member("c", "unanswered")], "delete b forget none"],
    ["orphan delete of a name with a writer", orphan, [member("a", "writer"), member("b", "mirror")], "refuse invalid"],
    ["orphan delete with no slot left", orphan, [member("b", "empty")], "refuse no_mirror"],
  ])("%s", (_, input, members, expected) => {
    expect(outcome(input, members)).toBe(expected);
  });

  it("names the copy and the confirmation in the only-copy refusal", () => {
    expect(planFromCopies(remove("fsn-2"), [member("fsn-2", "mirror")])).toEqual({
      ok: false,
      refusal: { code: "confirm_required", message: "data-fsn-2 is data's only copy; volume mirror rm data-fsn-2 --confirm data" },
    });
  });

  it("names the Server without managed storage", () => {
    expect(planFromCopies(mirror("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "empty", { pool: false })])).toEqual({
      ok: false,
      refusal: { code: "no_pool", message: "fsn-2 has no managed volume storage yet" },
    });
  });
});
