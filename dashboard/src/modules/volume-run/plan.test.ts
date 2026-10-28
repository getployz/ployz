import { describe, expect, it } from "vitest";
import type { MachineId, VolumeCopy } from "@ployz/sdk";
import { expectOf, observedEpoch, type PlanInput, planForwardRelease, planFromCopies, restoreSource, type Role, roleOf } from "#/modules/volume-run/plan";
import type { AnsweredMember, Member } from "#/modules/volume-run/volume-run";
import type { DeploymentsSeen } from "#/modules/volume-run/resize";
import { COPIES, type Copy, member } from "#/modules/volume-run/volume-run.test-fixture";

const GB = 1_000_000_000;
const data = { volumeName: "data", orphan: false } as const;
const mirror = (to: string): PlanInput => ({ kind: "mirror", args: { to }, ...data });
const sync = (full = false): PlanInput => ({ kind: "sync", args: { full }, ...data });
const remove = (slot: string | null, confirmed: string | null = null): PlanInput =>
  ({ kind: "delete_mirror", args: { slot, confirmed_name: confirmed, role: "any" }, ...data });
const move = (to: string): PlanInput => ({ kind: "move", args: { to }, ...data });
const release: PlanInput = { kind: "release", args: {}, ...data };
const restore = (from: string): PlanInput => ({ kind: "restore", args: { from }, ...data });
const seed: PlanInput = { kind: "seed", args: { into: { volumeId: "v2", dockerVolume: "ns_vol-v2", name: "data-1027" }, guid: "11", name: "w-1-4" }, ...data };
const resize = (gigabytes: number, size: { recordedBytes?: number; refquotaBytes?: number } = {}): PlanInput =>
  ({ kind: "resize", args: { maximum_bytes: gigabytes * GB }, ...data, recordedBytes: 10 * GB, refquotaBytes: 10 * GB, ...size });
const orphan: PlanInput = { kind: "delete_mirror", args: { slot: null, confirmed_name: null }, ...data, volumeName: "ns_vol-1", orphan: true };

const bare = (server: string, epoch: number): Member =>
  ({ machine: member(server, "cut").machine, address: "fdcc::1", answered: true, view: { copy: { ...COPIES.cut, epoch, newest: null } }, floorless: false });

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
    case "resize":
      return `${phase.direction} ${phase.from / GB}->${phase.to / GB} GB: ${phase.steps.map((step) => (step.kind === "record" ? "record" : `${step.role} ${step.member.machine.name}`)).join(", ")}`;
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
    ["mirror onto an empty Server", mirror("b"), [member("a", "writer"), member("b", "empty")], "mirror a->b"],
    ["mirror onto its mirror", mirror("b"), [member("a", "writer"), member("b", "mirror")], "mirror a->b"],
    ["mirror onto the writer", mirror("a"), [member("a", "writer"), member("b", "empty")], "refuse already_on"],
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
    ["orphan delete destroys every slot", orphan, [member("b", "mirror"), member("c", "pulling"), member("d", "empty")], "delete b,c forget none"],
    ["orphan delete ignores an unanswered Server", orphan, [member("b", "mirror"), member("c", "unanswered")], "delete b forget none"],
    ["orphan delete of a name with a writer", orphan, [member("a", "writer"), member("b", "mirror")], "refuse invalid"],
    ["orphan delete of a name with a cut root", orphan, [member("a", "cut"), member("b", "mirror")], "refuse invalid"],
    ["orphan delete destroys an unadopted seed root and its slots", orphan, [member("a", "seed"), member("b", "mirror")], "delete a,b forget none"],
    ["orphan delete of a name with a seed root beside a writer", orphan, [member("a", "seed"), member("b", "writer")], "refuse invalid"],
    ["orphan delete of a root an older daemon reports", orphan, [member("a", "rootWithoutSeed")], "refuse invalid"],
    ["orphan delete with no slot left", orphan, [member("b", "empty")], "refuse no_mirror"],

    ["move from a writer onto an empty Server runs rounds", move("b"), [member("a", "writer"), member("b", "empty")], "move a->b at rounds"],
    ["move from a writer onto its mirror runs rounds", move("b"), [member("a", "writer"), member("b", "mirror")], "move a->b at rounds"],
    ["move from a writer onto a mirror mid-pull runs rounds", move("b"), [member("a", "writer"), member("b", "pulling")], "move a->b at rounds"],
    ["move again once the root is read-only continues the cut", move("b"), [member("a", "cut"), member("b", "mirror")], "move a->b at cut"],
    ["move again once the root is read-only and the target behind continues the cut", move("b"), [member("a", "cut"), member("b", "behind")], "move a->b at cut"],
    ["move again once only slots remain and the target is behind pulls the final", move("b"), [member("a", "mirror"), member("b", "behind")], "move a->b at final"],
    ["move again once only slots remain and the target is mid-pull pulls the final", move("b"), [member("a", "mirror"), member("b", "pulling")], "move a->b at final"],
    ["move again once only the source slot remains pulls the final", move("b"), [member("a", "mirror"), member("b", "empty")], "move a->b at final"],
    ["move again once the target slot holds the source's newest starts", move("b"), [member("a", "mirror"), member("b", "mirror")], "move a->b at start"],
    ["move again once the target writes at the source's newest leaves the run to find the cut source", move("b"), [member("a", "final"), member("b", "writer")], "landed on b behind a"],
    ["move to the writer beside mirrors at its newest names them all, not only a final snapshot", move("b"), [member("a", "mirror"), member("b", "writer"), member("c", "final")], "landed on b behind a,c"],
    ["move onto the writer with its mirror behind", move("a"), [member("a", "writer"), member("b", "behind")], "refuse already_on"],
    ["move onto the writer alone", move("a"), [member("a", "writer")], "refuse already_on"],
    ["move onto a root mid-cut", move("a"), [member("a", "cut"), member("b", "mirror")], "refuse volume_switching"],
    ["move with a mirror elsewhere", move("c"), [member("a", "writer"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["move a cut toward another Server", move("c"), [member("a", "cut"), member("b", "mirror"), member("c", "empty")], "refuse second_mirror"],
    ["move with two slots besides the target", move("b"), [member("a", "mirror"), member("b", "empty"), member("c", "behind")], "refuse second_mirror"],
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
    ["restore the writable writer", restore("a"), [member("a", "writer"), member("b", "empty")], "refuse already_on"],
    ["restore while another Server holds the writer", restore("b"), [member("a", "writer"), member("b", "mirror")], "refuse another_copy"],
    ["restore while another Server holds a cut root", restore("b"), [member("a", "cut"), member("b", "mirror")], "refuse another_copy"],
    ["restore from a Server with no copy", restore("b"), [member("a", "empty"), member("b", "empty")], "refuse no_copy"],
    ["restore from a Server not in the cluster", restore("z"), [member("b", "mirror")], "refuse invalid"],
    ["restore with a Server unanswered", restore("b"), [member("a", "unanswered"), member("b", "mirror")], "refuse unanswered"],
    ["a returning root does not block Restore's refusal", restore("b"), [member("a", "writer", { epoch: 3 }), member("b", "writer", { epoch: 8 })], "refuse already_on"],

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

  it.each([["move", move("fsn-3")], ["mirror", mirror("fsn-3")]])("names the copy on a third Server and its removal when a %s finds one", (_, input) => {
    expect(planFromCopies(input, [member("fsn-1", "writer"), member("fsn-2", "mirror"), member("fsn-3", "empty")])).toEqual({
      ok: false,
      refusal: { code: "second_mirror", message: "data already has a copy on fsn-2, data-fsn-2", next: "ployz volume mirror rm data-fsn-2" },
    });
  });

  it.each([["move", move("fsn-3")], ["mirror", mirror("fsn-3")]])("names an old copy's removal with --if-old-copy when a %s finds one on a third Server", (_, input) => {
    expect(planFromCopies(input, [member("fsn-1", "writer"), member("fsn-2", "old"), member("fsn-3", "empty")])).toEqual({
      ok: false,
      refusal: { code: "second_mirror", message: "data already has a copy on fsn-2, data-fsn-2", next: "ployz volume mirror rm data-fsn-2 --if-old-copy" },
    });
  });

  it("refuses a delete admitted for an old copy once its slot holds the mirror, and deletes nothing", () => {
    const oldCopy = (slot: string): PlanInput => ({ kind: "delete_mirror", args: { slot, confirmed_name: null, role: "old_copy" }, ...data });
    expect(planFromCopies(oldCopy("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "mirror")])).toEqual({
      ok: false,
      refusal: { code: "role_changed", message: "data-fsn-2 is no longer an old copy, so nothing was deleted" },
    });
    expect(outcome(oldCopy("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "old")])).toBe("delete fsn-2 forget fsn-1");
    expect(outcome(remove("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "mirror")])).toBe("delete fsn-2 forget fsn-1");
    expect(outcome(remove("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "old")])).toBe("delete fsn-2 forget fsn-1");
  });

  it("leaves a slot whose Server reports no role to that Server's own check", () => {
    const { role: _, ...unreported } = COPIES.mirror;
    const silent: Member = { machine: member("fsn-2", "mirror").machine, address: "fdcc::1", answered: true, view: { copy: unreported }, floorless: false };
    const input: PlanInput = { kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "old_copy" }, ...data };
    expect(outcome(input, [member("fsn-1", "writer"), silent])).toBe("delete fsn-2 forget fsn-1");
  });

  it("keeps --if-old-copy in the only-copy refusal of a delete admitted for an old copy", () => {
    const input: PlanInput = { kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "old_copy" }, ...data };
    expect(planFromCopies(input, [member("fsn-2", "old")])).toEqual({
      ok: false,
      refusal: { code: "confirm_required", message: "data-fsn-2 is data's only copy; volume mirror rm data-fsn-2 --if-old-copy --confirm data" },
    });
  });

  it("names release, not the removal mirror rm refuses, when a move from a stopped cut finds a copy on a third Server", () => {
    expect(planFromCopies(move("fsn-3"), [member("fsn-1", "cut"), member("fsn-2", "mirror"), member("fsn-3", "empty")])).toEqual({
      ok: false,
      refusal: { code: "second_mirror", message: "data already has a copy on fsn-2, data-fsn-2", next: "ployz volume release data" },
    });
  });

  it("names the move onto the copy when a cut with no snapshot finds it on a third Server", () => {
    expect(planFromCopies(move("fsn-3"), [bare("fsn-1", 9), member("fsn-2", "mirror"), member("fsn-3", "empty")])).toEqual({
      ok: false,
      refusal: { code: "second_mirror", message: "data already has a copy on fsn-2, data-fsn-2", next: "ployz volume move data --to fsn-2" },
    });
  });

  it("names no next step when a cut with no snapshot has copies on the target and a third Server", () => {
    expect(planFromCopies(move("fsn-3"), [bare("fsn-1", 9), member("fsn-2", "mirror"), member("fsn-3", "mirror")])).toEqual({
      ok: false,
      refusal: { code: "second_mirror", message: "data already has a copy on fsn-2, data-fsn-2" },
    });
  });

  it.each([["release", release], ["sync", sync()], ["mirror rm", remove(null)]])("tells a %s facing two unfinished cuts to keep the newer and release it", (_, input) => {
    expect(planFromCopies(input, [member("fsn-1", "cut", { epoch: 8 }), member("fsn-2", "cut", { epoch: 9 })])).toEqual({
      ok: false,
      refusal: {
        code: "volume_switching",
        message: "data has unfinished cuts on fsn-1 and fsn-2, so release cannot pick a writer; ployz server rm fsn-1, then ployz volume release data",
      },
    });
  });

  it("keeps the mid-run refusal for a mirror rm beside cuts while a writer stands", () => {
    const members = [member("a", "writer", { epoch: 9 }), member("b", "cut", { epoch: 8 }), member("d", "cut", { epoch: 7 }), member("c", "mirror")];
    expect(planFromCopies(remove("c"), members)).toEqual({
      ok: false,
      refusal: { code: "volume_switching", message: "data is mid-run; ployz volume release data stops it" },
    });
    expect(outcome(release, members)).toBe("demote d below a");
  });

  it("names every older cut for server rm", () => {
    const members = [member("fsn-1", "cut", { epoch: 7 }), member("fsn-2", "cut", { epoch: 9 }), member("fsn-3", "cut", { epoch: 8 })];
    expect(planFromCopies(release, members)).toEqual({
      ok: false,
      refusal: {
        code: "volume_switching",
        message: "data has unfinished cuts on fsn-1 and fsn-2 and fsn-3, so release cannot pick a writer; ployz server rm fsn-1, then ployz server rm fsn-3, then ployz volume release data",
      },
    });
  });

  it("keeps the newer cut though it holds no snapshot, and removes every slot but the one it moves onto", () => {
    const members = [member("fsn-1", "cut", { epoch: 8 }), bare("fsn-2", 9), member("fsn-4", "behind"), member("fsn-3", "mirror")];
    expect(planFromCopies(release, members)).toEqual({
      ok: false,
      refusal: {
        code: "volume_switching",
        message: "data has unfinished cuts on fsn-1 and fsn-2, so release cannot pick a writer; ployz server rm fsn-1, then ployz server rm fsn-4, then ployz volume move data --to fsn-3",
      },
    });
  });

  it.each([
    ["release", release],
    ["sync", sync()],
    ["seed", seed],
    ["mirror rm", remove(null)],
    ["move onto its own Server", move("fsn-2")],
  ])("sends %s on a lone cut with no snapshot to move it onto its mirror's Server", (_, input) => {
    expect(planFromCopies(input, [bare("fsn-2", 9), member("fsn-3", "mirror")])).toEqual({
      ok: false,
      refusal: { code: "no_copy", message: "data on fsn-2 stopped before its first snapshot; ployz volume move data --to fsn-3" },
    });
  });

  it("keeps the tied cut that holds a snapshot and removes the other, not the mirror beside them", () => {
    const members = [bare("fsn-1", 9), member("fsn-2", "cut", { epoch: 9 }), member("fsn-3", "mirror")];
    expect(planFromCopies(release, members)).toEqual({
      ok: false,
      refusal: {
        code: "volume_switching",
        message: "data has unfinished cuts on fsn-1 and fsn-2, so release cannot pick a writer; ployz server rm fsn-1, then ployz volume release data",
      },
    });
  });

  it.each<[string, Member[], string, string]>([
    [
      "of three cuts with the top two tied, keeps the one that holds a snapshot",
      [member("fsn-1", "cut", { epoch: 7 }), member("fsn-2", "cut", { epoch: 9 }), bare("fsn-3", 9)],
      "volume_switching",
      "data has unfinished cuts on fsn-1 and fsn-2 and fsn-3, so release cannot pick a writer; ployz server rm fsn-1, then ployz server rm fsn-3, then ployz volume release data",
    ],
    [
      "of tied cuts with no snapshot, keeps the first by Server name and moves it onto the mirror",
      [bare("fsn-2", 9), bare("fsn-1", 9), member("fsn-3", "mirror")],
      "volume_switching",
      "data has unfinished cuts on fsn-2 and fsn-1, so release cannot pick a writer; ployz server rm fsn-2, then ployz volume move data --to fsn-3",
    ],
    [
      "moves a lone cut with no snapshot onto the first of two slots and removes the other",
      [bare("fsn-2", 9), member("fsn-4", "behind"), member("fsn-3", "mirror")],
      "no_copy",
      "data on fsn-2 stopped before its first snapshot; ployz server rm fsn-4, then ployz volume move data --to fsn-3",
    ],
  ])("%s", (_, members, code, message) => {
    expect(planFromCopies(release, members)).toEqual({ ok: false, refusal: { code, message } });
  });

  it("names both copies when two writers tie", () => {
    expect(planFromCopies(release, [member("fsn-1", "writer", { epoch: 4 }), member("fsn-2", "writer", { epoch: 4 })])).toEqual({
      ok: false,
      refusal: { code: "two_writers", message: "data has two writers, data-fsn-1 and data-fsn-2" },
    });
  });

  it("sends a pinned mirror or move to its Machine when another Server took the name", () => {
    const renamed = member("fsn-3", "empty", { id: "fsn-2" });
    const reused = member("fsn-2", "empty", { id: "fsn-9" });
    const pinned = { to: "fsn-2", machine: "fsn-2".padEnd(32, "0") as MachineId };
    const members = [member("fsn-1", "writer"), reused, renamed];
    expect(outcome({ kind: "mirror", args: pinned, volumeName: "data", orphan: false }, members)).toBe("mirror fsn-1->fsn-3");
    expect(outcome({ kind: "move", args: pinned, volumeName: "data", orphan: false }, members)).toBe("move fsn-1->fsn-3 at rounds");
    expect(outcome({ kind: "mirror", args: pinned, volumeName: "data", orphan: false }, [member("fsn-1", "writer"), reused]))
      .toBe("refuse invalid");
  });

  it("restores a pinned copy from its Machine when another Server took the name", () => {
    const renamed = member("fsn-3", "mirror", { id: "fsn-2" });
    const reused = member("fsn-2", "empty", { id: "fsn-9" });
    const pinned = { from: "fsn-2", machine: "fsn-2".padEnd(32, "0") as MachineId };
    expect(outcome({ kind: "restore", args: pinned, volumeName: "data", orphan: false }, [reused, renamed])).toBe("restore fsn-3 11");
    expect(outcome({ kind: "restore", args: pinned, volumeName: "data", orphan: false }, [reused])).toBe("refuse invalid");
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

  it("says a Server's report hasn't arrived, not that it did not answer, and names a silent one first", () => {
    const waiting: Member = { machine: { id: "fsn-9".padEnd(32, "0") as MachineId, name: "fsn-9" }, answered: false, incomplete: true };
    expect(planFromCopies(mirror("fsn-2"), [member("fsn-1", "writer"), member("fsn-2", "empty"), waiting])).toEqual({
      ok: false, refusal: { code: "unanswered", message: "fsn-9's report hasn't arrived; data's copies are unknown" },
    });
    expect(planFromCopies(mirror("fsn-2"), [member("fsn-1", "writer"), waiting, member("fsn-2", "unanswered")])).toMatchObject({
      ok: false, refusal: { message: "fsn-2 did not answer; data's copies are unknown" },
    });
  });

  it("ends with the lacking Server's sentence before any Server that did not answer", () => {
    const old: Member = { machine: member("fsn-3", "unanswered").machine, answered: false, lack: "Volume move needs a newer Ployz on fsn-3, which runs 0.2.4. Upgrade it to 0.2.5." };
    const members = [member("fsn-1", "writer"), member("fsn-2", "unanswered"), old];
    expect(planFromCopies(mirror("fsn-2"), members)).toEqual({
      ok: false,
      refusal: { code: "server_lacks", message: "Volume move needs a newer Ployz on fsn-3, which runs 0.2.4. Upgrade it to 0.2.5." },
    });
    expect(planFromCopies(remove("fsn-2"), members)).toMatchObject({ ok: false, refusal: { code: "server_lacks" } });
    expect(planFromCopies(move("fsn-2"), members)).toMatchObject({ ok: false, refusal: { code: "server_lacks" } });
  });
});

describe("plan a resize", () => {
  const copies = [member("fsn-1", "writer"), member("fsn-2", "mirror"), member("fsn-3", "mirror")];

  it("sets every mirror first, the primary last, then records", () => {
    expect(outcome(resize(20), copies)).toBe("grow 10->20 GB: mirror fsn-2, mirror fsn-3, primary fsn-1, record");
  });

  it("records a smaller size first, then sets the primary, then every mirror", () => {
    expect(outcome(resize(5), copies)).toBe("shrink 10->5 GB: record, primary fsn-1, mirror fsn-2, mirror fsn-3");
  });

  it("settles the size it already has on the primary, then every mirror, with no record", () => {
    const held = [member("fsn-1", "writer", { bound: 10 * GB }), member("fsn-2", "mirror", { bound: 20 * GB }), member("fsn-3", "mirror", { bound: 10 * GB })];
    expect(outcome(resize(10), held)).toBe("settle 10->10 GB: primary fsn-1, mirror fsn-2, mirror fsn-3");
  });

  it("sets a lone primary, then records", () => {
    expect(outcome(resize(20), [member("fsn-1", "writer")])).toBe("grow 10->20 GB: primary fsn-1, record");
  });

  it("is the same plan on every replay from the same copies", () => {
    const once = planFromCopies(resize(20), copies);
    expect(planFromCopies(resize(20), copies)).toEqual(once);
    expect(copies.map((each) => each.machine.name)).toEqual(["fsn-1", "fsn-2", "fsn-3"]);
  });

  it.each<[string, PlanInput, Member[], string]>([
    ["a Server lacking the verb", resize(20), [...copies, { machine: member("fsn-4", "unanswered").machine, answered: false, lack: "Volume resize needs a newer Ployz on fsn-4." }], "refuse server_lacks"],
    ["a Server that did not answer", resize(20), [...copies, member("fsn-4", "unanswered")], "refuse unanswered"],
    ["no writer", resize(20), [member("fsn-2", "mirror")], "refuse no_writer"],
    ["a cut in progress", resize(20), [member("fsn-1", "cut"), member("fsn-2", "pulling")], "refuse volume_switching"],
    ["two writers", resize(20), [member("fsn-1", "writer"), member("fsn-2", "writer")], "refuse two_writers"],
    ["an old copy beside the writer", resize(20), [member("fsn-1", "writer", { epoch: 2 }), member("fsn-2", "writer", { epoch: 1 })], "demote fsn-2 below fsn-1"],
    ["a Docker Volume", resize(20, { recordedBytes: 0, refquotaBytes: 0 }), copies, "refuse invalid"],
    ["a Volume no Deploy sized yet", resize(20, { recordedBytes: 0 }), copies, "refuse invalid"],
  ])("refuses or demotes with %s", (_, input, members, expected) => {
    expect(outcome(input, members)).toBe(expected);
  });

  it("names the Server that lacks the verb and why a Volume has no size to change", () => {
    const lacking: Member = { machine: member("fsn-4", "unanswered").machine, answered: false, lack: "Volume resize needs a newer Ployz on fsn-4." };
    expect(planFromCopies(resize(20), [...copies, lacking])).toMatchObject({ ok: false, refusal: { message: "Volume resize needs a newer Ployz on fsn-4." } });
    expect(planFromCopies(resize(20, { recordedBytes: 0, refquotaBytes: 0 }), copies)).toMatchObject({ ok: false, refusal: { message: "data uses Docker storage, which has no size to change; add a Managed Volume for a sized one" } });
    expect(planFromCopies(resize(20, { recordedBytes: 0 }), copies)).toMatchObject({ ok: false, refusal: { message: "data has no deployed size to change yet; deploy it first" } });
  });
});

describe("plan a resize of a Volume no Server holds (#3501)", () => {
  const seen = { latest: 3, inFlight: null, readAt: 1_000 };
  const unheld = (gigabytes: number, deployments: DeploymentsSeen = seen): PlanInput =>
    ({ kind: "resize", args: { maximum_bytes: gigabytes * GB }, ...data, recordedBytes: 10 * GB, refquotaBytes: 10 * GB, deployments });
  const empty = [member("fsn-1", "empty"), member("fsn-2", "empty")];
  const stateless = (server: string): Member => ({ machine: member(server, "unanswered").machine, answered: false, stateless: true });

  it("records the size alone, naming the newest Deployment it saw before asking the Servers", () => {
    expect(planFromCopies(unheld(20), empty)).toEqual({ ok: true, phase: { kind: "resize", direction: "grow", from: 10 * GB, to: 20 * GB, steps: [{ kind: "record" }], unheld: { latest_deployment: 3, looked_at: 1_000 } } });
    expect(outcome(unheld(4), empty)).toBe("shrink 10->4 GB: record");
    expect(outcome(unheld(10), empty)).toBe("settle 10->10 GB: ");
  });

  it("refuses while a Deployment is in flight, which may be creating the copy now", () => {
    expect(planFromCopies(unheld(20, { latest: 4, inFlight: { number: 4 }, readAt: 1_000 }), empty)).toMatchObject({ ok: false, refusal: {
      code: "deploying", message: "Deployment #4 is in flight; run ployz volume set data --size 20 again when it ends",
    } });
  });

  it("counts a Server reporting no managed storage as unanswered, since a missing zpool hides an imported pool's copy", () => {
    expect(planFromCopies(unheld(20), [...empty, stateless("fsn-3")])).toMatchObject({ ok: false, refusal: {
      code: "unanswered", message: "fsn-3 reports no managed storage, so whether it holds data is unknown; change its size once fsn-3 reports its storage, or after the next deploy creates it",
    } });
  });

  it("leaves a Server reporting no managed storage out of a resize that found the copy", () => {
    expect(outcome(resize(20), [member("fsn-1", "writer"), stateless("fsn-3")])).toBe("grow 10->20 GB: primary fsn-1, record");
  });

  it("refuses without the Deployments it read, and as before for a Docker Volume or one no Deploy sized", () => {
    expect(outcome(resize(20), empty)).toBe("refuse unobserved");
    expect(outcome(unheld(20), [...empty, member("fsn-3", "unanswered")])).toBe("refuse unanswered");
    expect(outcome({ kind: "resize", args: { maximum_bytes: 20 * GB }, ...data, recordedBytes: 0, refquotaBytes: 0, deployments: seen }, empty)).toBe("refuse invalid");
  });
});

const named = (members: readonly Member[], role: Role) => members.filter((each) => roleOf(each) === role).map((each) => each.machine.name).sort((a, b) => a.localeCompare(b));

/** The cut every remedy keeps: highest epoch, then holds a snapshot, then first by Server name. */
function keptCut(members: readonly Member[]) {
  const rank = (each: Member): [number, number] => {
    const copy = each.answered ? each.view.copy : null;
    return copy?.kind === "root" ? [copy.epoch, copy.newest === null ? 0 : 1] : [0, 0];
  };
  const [kept] = members.filter((each) => roleOf(each) === "cutting").sort((a, b) => {
    const [ae, as] = rank(a);
    const [be, bs] = rank(b);
    return be - ae || bs - as || a.machine.name.localeCompare(b.machine.name);
  });
  if (kept === undefined) throw new Error("no cut to keep");
  return kept;
}

function nextStep(message: string) {
  const step = new RegExp([
    /^data (?:has unfinished cuts on ([\w-]+(?: and [\w-]+)*), so release cannot pick a writer|on ([\w-]+) stopped before its first snapshot); /.source,
    /((?:ployz server rm [\w-]+, then )*)ployz volume (?:(release) data|(move) data --to (<another server>|[\w-]+))$/.source,
  ].join("")).exec(message);
  if (step === null) throw new Error(`no step in: ${message}`);
  const [, cuts, stopped, removed, released, moved, to] = step;
  return {
    stem: cuts === undefined ? "stopped" : "cuts",
    about: (cuts ?? stopped ?? "").split(" and ").sort(),
    gone: [...(removed ?? "").matchAll(/ployz server rm ([\w-]+)/g)].flatMap(([, server]) => server ?? []).sort(),
    verb: released ?? moved,
    to,
  };
}

describe("read the next step a refusal names", () => {
  const first = "data has unfinished cuts on fsn-1 and fsn-2, so release cannot pick a writer";
  const cuts = `${first}; ployz server rm fsn-1, then ployz volume release data`;
  const stopped = "data on fsn-2 stopped before its first snapshot; ployz volume move data --to fsn-3";
  it.each([
    ["a step inside the first clause", cuts, first, "ployz volume release data"],
    ["a command with no ployz prefix in the first clause", cuts, "so release cannot", "so volume release data cannot"],
    ["a tab after ployz in the first clause", cuts, first, "ployz\tvolume release data"],
    ["a capitalized command as the first clause", cuts, first, "Ployz volume release data first"],
    ["the mid-run first clause, which follow does not read", stopped, "on fsn-2 stopped before its first snapshot", "is mid-run"],
    ["a target on a release", cuts, "release data", "release data --to fsn-9"],
    ["a move with no target", stopped, " --to fsn-3", ""],
    ["text after the step", cuts, "release data", "release data stops it"],
  ])("rejects %s and accepts the step without it", (_, step, from, to) => {
    const message = step.replace(from, to);
    expect(message).not.toBe(step);
    expect(() => nextStep(step)).not.toThrow();
    expect(() => nextStep(message)).toThrow("no step in");
  });
});

function follow(input: PlanInput, members: readonly Member[]) {
  const planned = planFromCopies(input, members);
  if (planned.ok) throw new Error(`planned ${planned.phase.kind}, not a refusal`);
  const kept = keptCut(members);
  const copy = kept.answered ? kept.view.copy : null;
  const cutting = named(members, "cutting");
  const slots = named(members, "mirror");
  const verb = copy?.kind === "root" && copy.newest !== null ? "release" : "move";
  const expected = {
    stem: cutting.length === 1 ? "stopped" : "cuts",
    about: cutting,
    gone: [...cutting.filter((name) => name !== kept.machine.name), ...(verb === "move" ? slots.slice(1) : [])].sort(),
    verb,
    to: verb === "move" ? (slots[0] ?? "<another server>") : undefined,
  };
  expect(nextStep(planned.refusal.message)).toEqual(expected);
  const left = members.filter((each) => !expected.gone.includes(each.machine.name));
  if (verb === "release") {
    expect(outcome(release, left)).toBe(`release cut ${kept.machine.name}`);
    return;
  }
  const target = slots[0] ?? left.find((each) => roleOf(each) === "empty")?.machine.name ?? "no empty Server";
  expect(outcome(move(target), left)).toBe(`move ${kept.machine.name}->${target} at cut`);
  if (slots.length > 0) expect(outcome(move(named(left, "empty")[0] ?? "no empty Server"), left)).toBe("refuse second_mirror");
}

describe("follow every next step a refusal names", () => {
  // `Required` is shallow. This pins every leaf at every depth, so a new optional field fails typecheck here until each
  // twin sets it, and `tied` then sees it.
  type Complete<T> = T extends string | number | bigint | boolean | null | undefined ? T : { [K in keyof T]-?: Complete<T[K]> };
  const twin = (id: string, floorless: boolean, copy: Complete<VolumeCopy>): AnsweredMember => ({
    machine: { id: id.padEnd(32, "0") as MachineId, name: "twin" }, address: `fdcc::${id}`, answered: true, view: { copy }, floorless, boundBytes: floorless ? GB : null,
  } satisfies Required<AnsweredMember>);
  const at = (name: string, each: AnsweredMember): Member => ({ ...each, machine: { ...each.machine, name } });
  const newest = COPIES.mirror.newest;
  const earlier = { ...COPIES.behind.newest, created_unix_seconds: newest.created_unix_seconds - 60 };
  const slotA = twin("a", false, { ...COPIES.behind, newest: earlier });
  const slotB = twin("b", true, { ...COPIES.mirror, slot: "2", resume_token: "tok", pull: { target: newest.name, failure: "no space" }, role: "old_copy" });
  const bareA = twin("c", false, { ...COPIES.cut, epoch: 9, newest: null });
  const bareB = twin("d", true, { ...COPIES.cut, epoch: 9, newest: null });
  const heldA = twin("c", false, { ...COPIES.cut, epoch: 9, newest: earlier });
  const heldB = twin("d", true, { ...COPIES.cut, epoch: 9 });

  type Tree = string | number | bigint | boolean | null | undefined | { readonly [key: string]: Tree };
  const tied = (a: Tree, b: Tree, path = ""): string[] => {
    if (!(a instanceof Object) && !(b instanceof Object)) return Object.is(a, b) ? [path] : [];
    const keys = new Set([...Object.keys(a instanceof Object ? a : {}), ...Object.keys(b instanceof Object ? b : {})]);
    const read = (value: Tree, key: string): Tree => (value instanceof Object ? value[key] : undefined);
    return [...keys].flatMap((key) => tied(read(a, key), read(b, key), path === "" ? key : `${path}.${key}`)).sort();
  };

  it("pairs twins that tie only where they must", () => {
    // A leaf may tie only if it makes the member a slot or a cut, if the settled rule ranks it ahead of the name, or if
    // only orphan deletes read it, which follow never plans. `machine.name` is the placeholder each row replaces.
    expect(tied(slotA, slotB)).toEqual(["answered", "machine.name", "view.copy.kind", "view.copy.readonly"]);
    const cut = ["answered", "machine.name", "view.copy.epoch", "view.copy.kind", "view.copy.newest", "view.copy.readonly", "view.copy.seed"];
    expect(tied(bareA, bareB)).toEqual(cut);
    expect(tied(heldA, heldB)).toEqual(cut.filter((leaf) => leaf !== "view.copy.newest"));
  });

  // Each pair is the same two twins with only their names swapped. The twins differ in every leaf `tied` does not list,
  // so a pick by any of those leaves, alone or ahead of the name, keeps a different Server in one of the two rows.
  const beside: [string, Member[]][] = [
    ["no slot", []],
    ["one slot", [member("fsn-3", "mirror")]],
    ["two slots", [at("fsn-4", slotA), at("fsn-3", slotB)]],
    ["two slots listed by name", [at("fsn-3", slotA), at("fsn-4", slotB)]],
  ];
  const cuts: [string, Member[]][] = [
    ["a newer cut with a snapshot", [member("fsn-1", "cut", { epoch: 8 }), member("fsn-2", "cut", { epoch: 9 })]],
    ["a newer cut with no snapshot", [member("fsn-1", "cut", { epoch: 8 }), bare("fsn-2", 9)]],
    ["a lone cut with no snapshot", [bare("fsn-2", 9)]],
    ["tied cuts with no snapshot", [at("fsn-2", bareA), at("fsn-1", bareB)]],
    ["tied cuts with no snapshot listed by name", [at("fsn-1", bareA), at("fsn-2", bareB)]],
    ["tied cuts with a snapshot", [at("fsn-2", heldA), at("fsn-1", heldB)]],
    ["tied cuts with a snapshot listed by name", [at("fsn-1", heldA), at("fsn-2", heldB)]],
  ];
  const kinds: [string, PlanInput][] = [
    ["release", release],
    ["sync", sync()],
    ["mirror", mirror("fsn-9")],
    ["seed", seed],
    ["mirror rm", remove(null)],
    ["move onto the newest cut's Server", move("fsn-2")],
  ];
  const fourth = member("fsn-9", "empty");

  it.each<[string, PlanInput, Member[]]>([
    ...beside.flatMap(([slots, each]) => cuts.flatMap(([state, held]) => kinds.map(([kind, input]): [string, PlanInput, Member[]] =>
      [`${kind} on ${state} beside ${slots}`, input, [...held, ...each, fourth]]))),
    ["a tie where one cut holds a snapshot", release, [bare("fsn-1", 9), member("fsn-2", "cut", { epoch: 9 }), fourth]],
    ["three cuts with the top two tied", release, [member("fsn-1", "cut", { epoch: 7 }), member("fsn-2", "cut", { epoch: 9 }), bare("fsn-3", 9), fourth]],
  ])("%s", (_, input, members) => {
    follow(input, members);
  });
});

describe("follow every release a refusal names on its own", () => {
  const stops = "data is mid-run; ployz volume release data stops it";
  const short = "data has no writer; ployz volume release data ends a move cut short";
  const cut = member("fsn-2", "cut", { epoch: 9 });
  const slots = [member("fsn-4", "behind"), member("fsn-3", "mirror")];
  const fourth = member("fsn-9", "empty");

  it.each<[string, PlanInput, Member[], string, string]>([
    ["mirror rm beside a lone cut that holds a snapshot", remove(null), [cut, member("fsn-3", "mirror")], stops, "release cut fsn-2"],
    ["mirror rm beside a writer and a cut", remove(null), [member("fsn-1", "writer", { epoch: 10 }), cut, member("fsn-3", "mirror")], stops, "demote fsn-2 below fsn-1"],
    ["seed on a cut", seed, [cut], stops, "release cut fsn-2"],
    ["sync on a cut", sync(), [cut, member("fsn-3", "mirror")], stops, "release cut fsn-2"],
    ["mirror from a cut", mirror("fsn-9"), [cut, fourth], stops, "release cut fsn-2"],
    ["move onto the cut's own Server", move("fsn-2"), [cut, member("fsn-3", "mirror")], stops, "release cut fsn-2"],
    ["sync with no writer beside a slot", sync(), [member("fsn-3", "mirror")], short, "release slots fsn-3"],
    ["mirror with no writer beside two slots", mirror("fsn-9"), [...slots, fourth], short, "release slots fsn-4,fsn-3"],
    ["move with no writer beside two slots", move("fsn-9"), [...slots, fourth], "data has copies on data-fsn-4 and data-fsn-3; ployz volume release data first", "release slots fsn-4,fsn-3"],
  ])("%s", (_, input, members, message, released) => {
    const planned = planFromCopies(input, members);
    if (planned.ok) throw new Error(`planned ${planned.phase.kind}, not a refusal`);
    expect(planned.refusal.message).toBe(message);
    expect(outcome(release, members)).toBe(released);
  });
});

describe("restoreSource", () => {
  const newer = (server: string): Member => {
    const slot = member(server, "mirror");
    if (!slot.answered || slot.view.copy?.newest == null) throw new Error("a mirror holds a snapshot");
    return { ...slot, view: { copy: { ...slot.view.copy, newest: { ...slot.view.copy.newest, created_unix_seconds: slot.view.copy.newest.created_unix_seconds + 60 } } } };
  };

  it("names the stopped copy with the newest snapshot, never a writer", () => {
    expect(restoreSource([member("fsn-1", "writer"), member("fsn-2", "mirror"), newer("fsn-3")])).toBe("fsn-3");
    expect(restoreSource([member("fsn-1", "cut")])).toBe("fsn-1");
  });

  it("names none when no stopped copy holds a snapshot", () => {
    expect(restoreSource([member("fsn-1", "writer"), member("fsn-2", "empty"), member("fsn-3", "seed")])).toBeUndefined();
  });

  it("names no slot when a third Server that may hold another did not answer", () => {
    expect(restoreSource([newer("fsn-2"), member("fsn-3", "unanswered")])).toBeUndefined();
  });

  it("names none when two different snapshots are newest within the same second", () => {
    expect(restoreSource([member("fsn-2", "mirror"), member("fsn-3", "behind")])).toBeUndefined();
  });

  it("names either copy when the newest snapshot is the same one on both", () => {
    expect(["fsn-2", "fsn-3"]).toContain(restoreSource([member("fsn-2", "mirror"), member("fsn-3", "cut")]));
  });
});

describe("planForwardRelease", () => {
  const B = member("fsn-2", "final").machine;
  const lacking: Member = { machine: member("fsn-1", "unanswered").machine, answered: false, lack: "Volume move needs a newer Ployz on fsn-1." };
  const servers = (machines: ReadonlyArray<{ readonly name: string }>) => machines.map((machine) => machine.name).join(",") || "none";
  const forward = (members: Member[]) => {
    const planned = planForwardRelease("data", B, members);
    switch (planned.kind) {
      case "ready":
        return `ready on ${planned.target.machine.name}, removing on ${servers(planned.removeOn)}`;
      case "waiting":
        return `waiting on ${servers(planned.servers)}, removing on ${servers(planned.removeOn)}`;
      case "blocked":
        return `blocked: ${planned.message}`;
    }
  };

  it.each<[string, Member[], string]>([
    ["B removed", [member("fsn-1", "mirror")], "blocked: data's writer was on fsn-2 (removed). Restore from fsn-1's copy: ployz volume restore data --from fsn-1"],
    ["B removed, no copy left", [member("fsn-1", "empty")], "blocked: data's writer was on fsn-2 (removed). No other server's copy is known to hold its newest data."],
    ["a Server lacking the switch", [lacking, member("fsn-2", "final"), member("fsn-3", "unanswered")], "blocked: Volume move needs a newer Ployz on fsn-1."],
    ["a silent A while B is a slot", [member("fsn-1", "unanswered"), member("fsn-2", "final"), member("fsn-3", "mirror")], "waiting on fsn-1, removing on fsn-1,fsn-3"],
    ["a silent B", [member("fsn-1", "mirror"), member("fsn-2", "unanswered")], "waiting on fsn-2, removing on fsn-1"],
    ["B with no copy", [member("fsn-1", "mirror"), member("fsn-2", "empty")], "blocked: data's Move committed, but fsn-2 holds no copy to start. Restore from fsn-1's copy: ployz volume restore data --from fsn-1"],
    ["B with no snapshot", [member("fsn-1", "empty"), member("fsn-2", "seed")], "blocked: data's Move committed, but fsn-2 holds no copy to start. No other server's copy is known to hold its newest data."],
    ["a writer outside B", [member("fsn-1", "writer"), member("fsn-2", "final")], "blocked: data-fsn-1 is still a writer copy, so data can't finish on fsn-2; ployz volume restore data --from fsn-1 keeps fsn-1's data"],
    ["a cut root outside B", [member("fsn-1", "cut"), member("fsn-2", "final")], "blocked: data has an unfinished cut on fsn-1, so release cannot finish it on fsn-2; ployz server rm fsn-1, then ployz volume release data"],
    ["B a slot, the rest stopped", [member("fsn-1", "mirror"), member("fsn-2", "final"), member("fsn-3", "empty")], "ready on fsn-2, removing on fsn-1,fsn-3"],
    ["B already the writer", [member("fsn-1", "mirror"), member("fsn-2", "writer", { epoch: 4 })], "ready on fsn-2, removing on fsn-1"],
  ])("%s", (_, members, expected) => {
    expect(forward(members)).toBe(expected);
  });

  it("names steps the planner accepts when cuts outside B began at different epochs, beside an unrelated mirror", () => {
    const members = [member("fsn-1", "cut", { epoch: 8 }), member("fsn-3", "cut", { epoch: 9 }), member("fsn-2", "final"), member("fsn-4", "mirror")];
    const planned = planForwardRelease("data", B, members);
    if (planned.kind !== "blocked") throw new Error(`planned ${planned.kind}, not a refusal`);
    expect(planned.message).toBe("data has unfinished cuts on fsn-1 and fsn-3, so release cannot finish it on fsn-2; ployz server rm fsn-1, then ployz server rm fsn-3, then ployz volume release data");
    const gone = [...planned.message.matchAll(/ployz server rm ([\w-]+), then /g)].flatMap(([, server]) => server ?? []);
    expect(forward(members.filter((each) => !gone.includes(each.machine.name)))).toBe("ready on fsn-2, removing on fsn-4");
  });
});
