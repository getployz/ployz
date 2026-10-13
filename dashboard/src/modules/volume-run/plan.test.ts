import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { expectOf, observedEpoch, type PlanInput, planFromCopies, type Role, roleOf } from "#/modules/volume-run/plan";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES, type Copy, member } from "#/modules/volume-run/volume-run.test-fixture";

const mirror = (to: string): PlanInput => ({ kind: "mirror", args: { to }, volumeName: "data", orphan: false });
const sync = (full = false): PlanInput => ({ kind: "sync", args: { full }, volumeName: "data", orphan: false });
const remove = (slot: string | null, confirmed: string | null = null): PlanInput =>
  ({ kind: "delete_mirror", args: { slot, confirmed_name: confirmed }, volumeName: "data", orphan: false });
const move = (to: string): PlanInput => ({ kind: "move", args: { to }, volumeName: "data", orphan: false });
const release: PlanInput = { kind: "release", args: {}, volumeName: "data", orphan: false };
const restore = (from: string): PlanInput => ({ kind: "restore", args: { from }, volumeName: "data", orphan: false });
const seed: PlanInput = { kind: "seed", args: { into: { volumeId: "v2", dockerVolume: "ns_vol-v2", name: "data-1027" }, guid: "11", name: "w-1-4" }, volumeName: "data", orphan: false };
const orphan: PlanInput = { kind: "delete_mirror", args: { slot: null, confirmed_name: null }, volumeName: "ns_vol-1", orphan: true };

const names = (members: readonly Member[]) => (members.length === 0 ? "none" : members.map((each) => each.machine.name).join(","));

function outcome(input: PlanInput, members: Member[]) {
  const planned = planFromCopies(input, members);
  if (!planned.ok) return `refuse ${planned.refusal.code}`;
  const { phase } = planned;
  switch (phase.kind) {
    case "mirror":
      return `mirror ${phase.writer.machine.name}->${phase.target.machine.name}`;
    case "sync":
      return `sync ${phase.writer.machine.name}->${phase.mirror.machine.name}${phase.full ? " full" : ""}`;
    case "delete_mirror":
      return `delete ${names(phase.destroy)} forget ${phase.forget?.machine.name ?? "none"}`;
    case "move":
      return `move ${phase.writer.machine.name}->${phase.target.machine.name} at ${phase.start}`;
    case "landed":
      return `landed on ${phase.target.machine.name} behind ${names(phase.behind)}`;
    case "release": {
      const { from } = phase;
      if (from.kind === "cut") return `release cut ${from.root.machine.name}`;
      if (from.kind === "writer") return `release writer ${from.writer.machine.name} behind ${names(from.behind)}`;
      return `release slots ${names(from.slots)}`;
    }
    case "restore":
      return `restore ${phase.from.machine.name}${phase.lostAfter === null ? "" : ` ${phase.lostAfter.guid}`}`;
    case "demote":
      return `demote ${phase.old.machine.name} below ${phase.writer.machine.name}`;
    case "seed":
      return `seed on ${phase.source.machine.name}`;
  }
}

describe("roleOf", () => {
  it.each<[Copy, Role]>([
    ["writer", "writer"],
    ["cut", "cutting"],
    ["mirror", "mirror"],
    ["behind", "mirror"],
    ["pulling", "mirror"],
    ["empty", "empty"],
    ["unanswered", "unanswered"],
  ])("reads %s as %s", (copy, role) => {
    expect(roleOf(member("a", copy))).toBe(role);
  });
});

describe("expectOf", () => {
  it("expects a root at its epoch, a slot at its newest, and an empty slot as none", () => {
    expect(expectOf(null)).toEqual({ kind: "none" });
    expect(expectOf({ ...COPIES.writer, epoch: 4 })).toEqual({ kind: "writer", epoch: 4 });
    expect(expectOf(COPIES.mirror)).toEqual({ kind: "mirror", newest: "11", slot: "1" });
    expect(expectOf({ ...COPIES.mirror, newest: null })).toEqual({ kind: "none" });
  });
});

describe("observedEpoch", () => {
  it("takes the highest epoch from roots and from the run that named a newest snapshot", () => {
    expect(observedEpoch([member("a", "writer", { epoch: 2 }), member("b", "mirror"), member("c", "unanswered")])).toBe(2);
    const later = { ...member("b", "mirror"), view: { copy: { ...COPIES.mirror, newest: { name: "f-7-1", guid: "12", created_unix_seconds: 0 } } } };
    expect(observedEpoch([member("a", "writer", { epoch: 2 }), later])).toBe(7);
    expect(observedEpoch([member("a", "empty"), member("b", "unanswered")])).toBe(0);
  });

  it("reads no epoch from a departure snapshot's timestamp", () => {
    const departed = { ...member("b", "mirror"), view: { copy: { ...COPIES.mirror, newest: { name: "dep-1690000000", guid: "12", created_unix_seconds: 0 } } } };
    expect(observedEpoch([member("a", "writer", { epoch: 2 }), departed])).toBe(2);
  });
});

describe("plan_from_copies_table", () => {
  it.each<[string, PlanInput, Member[], string]>([
    ["mirror onto an empty Server", mirror("b"), [member("a", "writer"), member("b", "empty", { pool: true })], "mirror a->b"],
    ["mirror onto its mirror", mirror("b"), [member("a", "writer"), member("b", "mirror")], "mirror a->b"],
    ["mirror onto a Server with no Pool", mirror("b"), [member("a", "writer"), member("b", "empty", { pool: false })], "refuse no_pool"],
    ["mirror onto the writer", mirror("a"), [member("a", "writer"), member("b", "empty")], "refuse invalid"],
    ["mirror onto a Server not in the cluster", mirror("z"), [member("a", "writer"), member("b", "empty")], "refuse invalid"],
    ["mirror with a mirror elsewhere", mirror("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["mirror with a Server unanswered", mirror("b"), [member("a", "writer"), member("b", "empty"), member("c", "unanswered")], "refuse unanswered"],
    ["mirror with two writers at one epoch", mirror("c"), [member("a", "writer"), member("b", "writer"), member("c", "empty")], "refuse two_writers"],
    ["mirror while a cut holds the root", mirror("b"), [member("a", "cut"), member("b", "empty")], "refuse volume_switching"],
    ["mirror with no copy", mirror("b"), [member("a", "empty"), member("b", "empty")], "refuse no_writer"],
    ["mirror with only a slot", mirror("b"), [member("a", "mirror"), member("b", "empty")], "refuse no_writer"],

    ["sync onto its mirror", sync(), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "sync a->b"],
    ["sync --full onto its mirror", sync(true), [member("a", "writer"), member("b", "mirror")], "sync a->b full"],
    ["sync onto a mirror mid-pull", sync(), [member("a", "writer"), member("b", "pulling")], "sync a->b"],
    ["sync with no mirror", sync(), [member("a", "writer"), member("b", "empty")], "refuse no_mirror"],
    ["sync with two mirrors", sync(), [member("a", "writer"), member("b", "mirror"), member("c", "behind")], "refuse second_mirror"],
    ["sync with no writer", sync(), [member("b", "mirror")], "refuse no_writer"],
    ["sync while a cut holds the root", sync(), [member("a", "cut"), member("b", "mirror")], "refuse volume_switching"],
    ["sync with a Server unanswered", sync(), [member("a", "writer"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],

    ["delete one mirror forgets on the writer", remove("b"), [member("a", "writer"), member("b", "mirror")], "delete b forget a"],
    ["delete a mirror mid-pull", remove("b"), [member("a", "writer"), member("b", "pulling")], "delete b forget a"],
    ["delete every slot", remove(null), [member("a", "writer"), member("b", "mirror"), member("c", "behind")], "delete b,c forget a"],
    ["delete a Server with no slot", remove("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse no_mirror"],
    ["delete the only copy unconfirmed", remove("b"), [member("a", "empty"), member("b", "mirror")], "refuse confirm_required"],
    ["delete the only copy confirmed", remove("b", "data"), [member("a", "empty"), member("b", "mirror")], "delete b forget none"],
    ["delete the only copy confirmed wrongly", remove("b", "other"), [member("b", "mirror")], "refuse confirm_required"],
    ["delete with two writers at one epoch", remove("c"), [member("a", "writer", { epoch: 8 }), member("b", "writer", { epoch: 8 }), member("c", "mirror")], "refuse two_writers"],
    ["delete with an old writer beside the restored one", remove("c"), [member("a", "writer", { epoch: 3 }), member("b", "writer", { epoch: 8 }), member("c", "mirror")], "refuse two_writers"],
    ["delete while a cut holds the root", remove("b"), [member("a", "cut"), member("b", "mirror")], "refuse volume_switching"],
    ["delete with a Server unanswered", remove("b"), [member("a", "writer"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],
    ["orphan delete destroys every slot", orphan, [member("b", "mirror"), member("c", "pulling"), member("d", "empty", { pool: true })], "delete b,c forget none"],
    ["orphan delete ignores an unanswered Server", orphan, [member("b", "mirror"), member("c", "unanswered")], "delete b forget none"],
    ["orphan delete of a name with a writer", orphan, [member("a", "writer"), member("b", "mirror")], "refuse invalid"],
    ["orphan delete of a name with a cut root", orphan, [member("a", "cut"), member("b", "mirror")], "refuse invalid"],
    ["orphan delete destroys an unadopted seed root and its slots", orphan, [member("a", "seed"), member("b", "mirror")], "delete a,b forget none"],
    ["orphan delete of a name with a seed root beside a writer", orphan, [member("a", "seed"), member("b", "writer")], "refuse invalid"],
    ["orphan delete of a root an older daemon reports", orphan, [member("a", "old")], "refuse invalid"],
    ["orphan delete with no slot left", orphan, [member("b", "empty")], "refuse no_mirror"],

    ["move from a writer onto an empty Server runs rounds", move("b"), [member("a", "writer"), member("b", "empty", { pool: true })], "move a->b at rounds"],
    ["move from a writer onto its mirror runs rounds", move("b"), [member("a", "writer"), member("b", "mirror")], "move a->b at rounds"],
    ["move from a writer onto a mirror mid-pull runs rounds", move("b"), [member("a", "writer"), member("b", "pulling")], "move a->b at rounds"],
    ["move again once the root is read-only continues the cut", move("b"), [member("a", "cut"), member("b", "mirror")], "move a->b at cut"],
    ["move again once the root is read-only and the target behind continues the cut", move("b"), [member("a", "cut"), member("b", "behind")], "move a->b at cut"],
    ["move again once only slots remain and the target is behind pulls the final", move("b"), [member("a", "mirror"), member("b", "behind")], "move a->b at final"],
    ["move again once only slots remain and the target is mid-pull pulls the final", move("b"), [member("a", "mirror"), member("b", "pulling")], "move a->b at final"],
    ["move again once only the source slot remains pulls the final", move("b"), [member("a", "mirror"), member("b", "empty", { pool: true })], "move a->b at final"],
    ["move again once the target slot holds the source's newest starts", move("b"), [member("a", "mirror"), member("b", "mirror")], "move a->b at start"],
    ["move again once the target writes at the source's newest leaves the run to find the cut source", move("b"), [member("a", "final"), member("b", "writer")], "landed on b behind a"],
    ["move to the writer beside mirrors at its newest names them all, not only a final snapshot", move("b"), [member("a", "mirror"), member("b", "writer"), member("c", "final")], "landed on b behind a,c"],
    ["move onto the writer with its mirror behind", move("a"), [member("a", "writer"), member("b", "behind")], "refuse invalid"],
    ["move onto the writer alone", move("a"), [member("a", "writer")], "refuse invalid"],
    ["move onto a root mid-cut", move("a"), [member("a", "cut"), member("b", "mirror")], "refuse volume_switching"],
    ["move with a mirror elsewhere", move("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["move a cut toward another Server", move("c"), [member("a", "cut"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["move with two slots besides the target", move("b"), [member("a", "mirror"), member("b", "empty", { pool: true }), member("c", "behind")], "refuse second_mirror"],
    ["move onto a Server with no Pool", move("b"), [member("a", "writer"), member("b", "empty", { pool: false })], "refuse no_pool"],
    ["move the final onto a Server with no Pool", move("b"), [member("a", "mirror"), member("b", "empty", { pool: false })], "refuse no_pool"],
    ["move onto a Server not in the cluster", move("z"), [member("a", "writer")], "refuse invalid"],
    ["move with a Server unanswered", move("b"), [member("a", "cut"), member("b", "mirror"), member("c", "unanswered")], "refuse unanswered"],
    ["move with no copy", move("b"), [member("a", "empty"), member("b", "mirror")], "refuse no_writer"],

    ["release a writer with its mirror at the writer's newest", release, [member("a", "writer"), member("b", "mirror")], "release writer a behind b"],
    ["release a writer with its mirror behind", release, [member("a", "writer"), member("b", "behind")], "release writer a behind none"],
    ["release a writer alone", release, [member("a", "writer")], "release writer a behind none"],
    ["release a cut stopped on a read-only root", release, [member("a", "cut"), member("b", "mirror")], "release cut a"],
    ["release with only slots", release, [member("a", "mirror"), member("b", "behind"), member("c", "empty")], "release slots a,b"],
    ["release with no copy", release, [member("a", "empty")], "refuse no_writer"],
    ["release with a Server unanswered", release, [member("a", "cut"), member("b", "unanswered")], "refuse unanswered"],

    ["restore the only mirror", restore("b"), [member("a", "empty"), member("b", "mirror")], "restore b 11"],
    ["restore a slot mid-pull loses what it has not pulled", restore("b"), [member("b", "pulling")], "restore b 10"],
    ["restore a read-only root loses nothing", restore("a"), [member("a", "cut"), member("b", "empty")], "restore a"],
    ["restore the writable writer", restore("a"), [member("a", "writer"), member("b", "empty")], "refuse invalid"],
    ["restore while another Server holds the writer", restore("b"), [member("a", "writer"), member("b", "mirror")], "refuse another_copy"],
    ["restore while another Server holds a cut root", restore("b"), [member("a", "cut"), member("b", "mirror")], "refuse another_copy"],
    ["restore from a Server with no copy", restore("b"), [member("a", "empty"), member("b", "empty")], "refuse no_copy"],
    ["restore from a Server not in the cluster", restore("z"), [member("b", "mirror")], "refuse invalid"],
    ["restore with a Server unanswered", restore("b"), [member("a", "unanswered"), member("b", "mirror")], "refuse unanswered"],
    ["a returning root does not block Restore's refusal", restore("b"), [member("a", "writer", { epoch: 3 }), member("b", "writer", { epoch: 8 })], "refuse invalid"],

    ["seed on the writer beside its mirror", seed, [member("a", "writer"), member("b", "mirror")], "seed on a"],
    ["seed with no writer", seed, [member("b", "mirror")], "refuse no_writer"],
    ["seed from a root mid-cut", seed, [member("a", "cut"), member("b", "mirror")], "refuse volume_switching"],
    ["seed with two writers at one epoch", seed, [member("a", "writer", { epoch: 8 }), member("b", "writer", { epoch: 8 })], "refuse two_writers"],
    ["seed with a Server unanswered", seed, [member("a", "writer"), member("b", "unanswered")], "refuse unanswered"],

    ["mirror with a returning root below the restored one", mirror("c"), [member("a", "writer", { epoch: 7 }), member("b", "writer", { epoch: 8 }), member("c", "empty")], "demote a below b"],
    ["move with a returning root above in the list", move("c"), [member("b", "writer", { epoch: 8 }), member("a", "writer", { epoch: 7 }), member("c", "empty")], "demote a below b"],
    ["seed with a returning root", seed, [member("a", "writer", { epoch: 8 }), member("b", "writer", { epoch: 3 })], "demote b below a"],
    ["release with three roots demotes the lowest", release, [member("a", "writer", { epoch: 8 }), member("b", "writer", { epoch: 3 }), member("c", "writer", { epoch: 5 })], "demote b below a"],
    ["a read-only root above the writer's epoch is the old one", sync(), [member("a", "cut", { epoch: 9 }), member("b", "writer", { epoch: 8 }), member("c", "mirror")], "demote a below b"],
    ["a read-only root tied with the writer is the old one", release, [member("b", "writer", { epoch: 8 }), member("a", "cut", { epoch: 8 })], "demote a below b"],
    ["a read-only root beside a lower writer is the old one", mirror("c"), [member("a", "writer", { epoch: 3 }), member("b", "cut", { epoch: 8 }), member("c", "empty")], "demote b below a"],
    ["two read-only roots are mid-run, not two writers", release, [member("a", "cut", { epoch: 9 }), member("b", "cut", { epoch: 8 })], "refuse volume_switching"],
    ["two writers at the same epoch stay two writers", release, [member("a", "writer", { epoch: 8 }), member("b", "writer", { epoch: 8 })], "refuse two_writers"],
    ["a tie at the top stays two writers beside a lower root", release, [member("a", "writer", { epoch: 8 }), member("b", "writer", { epoch: 8 }), member("c", "writer", { epoch: 3 })], "refuse two_writers"],
  ])("%s", (_, input, members, expected) => {
    expect(outcome(input, members)).toBe(expected);
  });

  it("names the copy and the confirmation in the only-copy refusal", () => {
    expect(planFromCopies(remove("fsn-2"), [member("fsn-2", "mirror")])).toEqual({
      ok: false,
      refusal: { code: "confirm_required", message: "data-fsn-2 is data's only copy; volume mirror rm data-fsn-2 --confirm data" },
    });
  });

  it("names both copies when two writers tie", () => {
    expect(planFromCopies(release, [member("fsn-1", "writer", { epoch: 4 }), member("fsn-2", "writer", { epoch: 4 })])).toEqual({
      ok: false,
      refusal: { code: "two_writers", message: "data has two writers, data-fsn-1 and data-fsn-2" },
    });
  });

  it("sends a pinned mirror or move to its Machine when another Server took the name", () => {
    const renamed = { ...member("fsn-3", "empty", { pool: true }), machine: { id: "fsn-2".padEnd(32, "0") as MachineId, name: "fsn-3" } };
    const reused = { ...member("fsn-2", "empty", { pool: true }), machine: { id: "fsn-9".padEnd(32, "0") as MachineId, name: "fsn-2" } };
    const pinned = { to: "fsn-2", machine: "fsn-2".padEnd(32, "0") as MachineId };
    const members = [member("fsn-1", "writer"), reused, renamed];
    expect(outcome({ kind: "mirror", args: pinned, volumeName: "data", orphan: false }, members)).toBe("mirror fsn-1->fsn-3");
    expect(outcome({ kind: "move", args: pinned, volumeName: "data", orphan: false }, members)).toBe("move fsn-1->fsn-3 at rounds");
    expect(outcome({ kind: "mirror", args: pinned, volumeName: "data", orphan: false }, [member("fsn-1", "writer"), reused]))
      .toBe("refuse invalid");
  });

  it("names every other copy Restore would leave behind", () => {
    expect(planFromCopies(restore("fsn-2"), [member("fsn-1", "cut"), member("fsn-2", "mirror"), member("fsn-3", "behind")])).toEqual({
      ok: false,
      refusal: {
        code: "another_copy",
        message: "data also has copies on fsn-1, fsn-3; Restore needs data-fsn-2 to be the only copy, so remove the others with server rm or volume mirror rm first",
      },
    });
  });

  it("refuses to restore a slot that holds no snapshot", () => {
    const bare = { ...member("fsn-2", "mirror"), view: { copy: { ...COPIES.mirror, newest: null } } };
    expect(planFromCopies(restore("fsn-2"), [bare])).toEqual({
      ok: false,
      refusal: { code: "no_copy", message: "data-fsn-2 holds no snapshot to restore" },
    });
  });

  it("restores a departed copy from its departure snapshot", () => {
    const newest = { name: "dep-1690000000", guid: "12", created_unix_seconds: 0 };
    const departed = { ...member("fsn-2", "mirror"), view: { copy: { ...COPIES.mirror, newest } } };
    expect(planFromCopies(restore("fsn-2"), [departed])).toMatchObject({ ok: true, phase: { kind: "restore", lostAfter: newest } });
  });

  it("ends with the lacking Server's sentence before any Server that did not answer", () => {
    const old: Member = { machine: member("fsn-3", "unanswered").machine, answered: false, lack: "Volume move needs a newer Ployz on fsn-3. 0.2.5 is out." };
    const members = [member("fsn-1", "writer"), member("fsn-2", "unanswered"), old];
    expect(planFromCopies(mirror("fsn-2"), members)).toEqual({
      ok: false,
      refusal: { code: "server_lacks", message: "Volume move needs a newer Ployz on fsn-3. 0.2.5 is out." },
    });
    expect(planFromCopies(remove("fsn-2"), members)).toMatchObject({ ok: false, refusal: { code: "server_lacks" } });
    expect(planFromCopies(move("fsn-2"), members)).toMatchObject({ ok: false, refusal: { code: "server_lacks" } });
  });

  it("names the Server without managed storage", () => {
    expect(planFromCopies(mirror("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "empty", { pool: false })])).toEqual({
      ok: false,
      refusal: { code: "no_pool", message: "fsn-2 has no managed volume storage yet" },
    });
  });
});
