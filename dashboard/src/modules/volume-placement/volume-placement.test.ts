import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import {
  detachedCopies,
  drainPlacement,
  editPlacement,
  failover,
  nextPlacementStep,
  observePlacement,
  placementChanges,
  placementSteps,
  placementTargets,
  placementEntries,
  placementGroup,
  stagedOffServer,
  storageStep,
  targetPlacement,
  undoPlacement,
  type PlacementRow,
} from "./volume-placement";

const volume = { id: "v1", name: "pg-data" };
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const row = (mirrors: string[] | null, staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors === null ? null : mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map((name) => id(name)), deletes: (staged.deletes ?? []).map((name) => id(name)) },
  deploymentId: null,
});
const pooled = (server: string) => member(server, "empty", { pool: true });
const at = new Date(2026, 9, 10, 10, 36);
const mirrorAsOf = (server: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: "s", guid: "12", created_unix_seconds: when.getTime() / 1000 } } } };
};
const changes = (members: Member[], placement: PlacementRow | null) => placementChanges(volume, members, placement);
const review = (members: Member[], placement: PlacementRow | null) => placementGroup(volume, changes(members, placement)).rows
  .map(({ kind, name, currentValue, newValue, detail }) => ({ kind, name, currentValue, newValue, detail }));
const primaryMove = (from: string, to: string, detail?: string) =>
  ({ kind: "update", name: "Primary", currentValue: from, newValue: to, detail });
const steps = (members: Member[], placement: PlacementRow | null) => placementSteps(changes(members, placement));

describe("placementChanges", () => {
  it("is empty when nothing is staged", () => {
    expect(changes([member("web-1", "writer"), member("web-2", "mirror")], row(["web-2"]))).toEqual([]);
  });

  it("makes a move one change, even when it consumes the mirror it promotes", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const staged = changes(members, row(["web-2"], { primary: "web-2" }));
    expect(staged.map((change) => change.kind)).toEqual(["move"]);
    expect(review(members, row(["web-2"], { primary: "web-2" }))).toEqual([primaryMove("web-1", "web-2")]);
  });

  it("keeps a move to the mirror as the same move after the hand-over, never as removing that mirror", () => {
    const members = [member("web-1", "handed", { lease: 3 }), member("web-2", "promoted", { lease: 4 })];
    expect(changes(members, row(["web-2"], { primary: "web-2" })).map((change) => change.kind)).toEqual(["move"]);
    expect(review(members, row(["web-2"], { primary: "web-2" }))).toEqual([primaryMove("web-1", "web-2")]);
  });

  it("copies first when the target holds nothing, and leaves the mirror list alone", () => {
    const members = [member("web-1", "writer"), pooled("web-3")];
    expect(review(members, row([], { primary: "web-3" }))).toEqual([primaryMove("web-1", "web-3")]);
    expect(steps(members, row([], { primary: "web-3" }))).toEqual([{ kind: "move", args: { to: "web-3" } }]);
  });

  it("restores from the mirror when the primary does not answer, and says what is lost", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    const [change] = changes(members, row(["web-2"], { primary: "web-2" }));
    expect(change).toMatchObject({ kind: "move", how: "restore", lostAfter: at, from: { name: "web-1" } });
    expect(review(members, row(["web-2"], { primary: "web-2" }))).toEqual([primaryMove("web-1", "web-2", "Changes after 10:36 lost")]);
    expect(steps(members, row(["web-2"], { primary: "web-2" }))).toEqual([{ kind: "restore", args: { from: "web-2" } }]);
  });

  it("treats a copy not listed in mirrors as detached", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(detachedCopies(observePlacement(members), row([])).map((copy) => copy.server.name)).toEqual(["web-1"]);
    expect(detachedCopies(observePlacement(members), row(["web-1"]))).toEqual([]);
  });

  it("counts every copy as a mirror before the dashboard has a record", () => {
    expect(detachedCopies(observePlacement([member("web-1", "writer"), member("web-2", "mirror")]), null)).toEqual([]);
  });

  it("deletes a detached copy before adding a mirror, and the review says so", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")];
    const placement = row([], { primary: "web-2", mirrors: ["web-3"] });
    expect(review(members, placement)).toEqual([{ kind: "add", name: "Mirror", currentValue: "", newValue: "web-3", detail: "Deletes pg-data-old first" }]);
    expect(steps(members, placement)).toEqual([
      { kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null } },
      { kind: "mirror", args: { to: "web-3" } },
    ]);
  });

  it("adopts a detached copy as the mirror on its own Server without deleting it", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(steps(members, row([], { primary: "web-2", mirrors: ["web-1"] }))).toEqual([{ kind: "mirror", args: { to: "web-1" } }]);
  });

  it("deletes a staged detached copy and nothing else", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(review(members, row([], { primary: "web-2", deletes: ["web-1"] }))).toEqual([
      { kind: "remove", name: "Detached copy", currentValue: "web-1", newValue: "" },
    ]);
  });

  it("removes a mirror the user took off the list", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror")];
    expect(review(members, row(["web-2"], { primary: "web-1" }))).toEqual([{ kind: "remove", name: "Mirror", currentValue: "web-2", newValue: "" }]);
  });

  it("orders runs so deletions free the copy slot before a move, and a new mirror comes last", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    expect(steps(members, row(["web-2"], { primary: "web-2", mirrors: ["web-3"] }))).toEqual([
      { kind: "move", args: { to: "web-2" } },
      { kind: "mirror", args: { to: "web-3" } },
    ]);
    expect(steps(members, row(["web-2"], { primary: "web-3" }))).toEqual([
      { kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null } },
      { kind: "move", args: { to: "web-3" } },
    ]);
  });
});

describe("placementEntries", () => {
  const entries = (members: Member[], placement: PlacementRow | null) => placementEntries(volume, members, placement)
    .map(({ role, server, change, movedFrom, undo }) => ({ role, server: server.name, change, movedFrom: movedFrom?.name ?? null, undo: undo?.kind ?? null }));

  it("marks nothing when nothing is staged, even while the primary is silent", () => {
    expect(entries([member("web-1", "unanswered"), member("web-2", "mirror")], row(["web-2"]))).toEqual([
      { role: "primary", server: "web-1", change: null, movedFrom: null, undo: null },
      { role: "mirror", server: "web-2", change: null, movedFrom: null, undo: null },
    ]);
  });

  it("shows a staged move on the new Server, what it was, and the old copy it leaves detached", () => {
    expect(entries([member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-3" }))).toEqual([
      { role: "primary", server: "web-3", change: "update", movedFrom: "web-1", undo: "move" },
      { role: "detached", server: "web-1", change: "add", movedFrom: null, undo: null },
    ]);
  });

  it("strikes a detached copy a new mirror deletes first, with no Undo of its own", () => {
    expect(entries([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([], { primary: "web-2", mirrors: ["web-3"] }))).toEqual([
      { role: "primary", server: "web-2", change: null, movedFrom: null, undo: null },
      { role: "mirror", server: "web-3", change: "add", movedFrom: null, undo: "add_mirror" },
      { role: "detached", server: "web-1", change: "remove", movedFrom: null, undo: null },
    ]);
  });

  it("keeps a removed mirror and a deleted copy in the list, struck, each with its Undo", () => {
    expect(entries([member("web-1", "writer"), member("web-2", "mirror")], row(["web-2"], { primary: "web-1" }))[1])
      .toEqual({ role: "mirror", server: "web-2", change: "remove", movedFrom: null, undo: "remove_mirror" });
    expect(entries([member("web-2", "writer"), member("web-1", "mirror")], row([], { primary: "web-2", deletes: ["web-1"] }))[1])
      .toEqual({ role: "detached", server: "web-1", change: "remove", movedFrom: null, undo: "delete_copy" });
  });
});

describe("undoPlacement", () => {
  const undo = (members: Member[], placement: PlacementRow, kind: string) => {
    const change = changes(members, placement).find((one) => one.kind === kind);
    if (change === undefined) throw new Error(`no ${kind}`);
    return undoPlacement(volume, members, placement, change);
  };

  it("drops one change and keeps the rest staged", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const placement = row(["web-2"], { primary: "web-3" });
    expect(undo(members, placement, "move")).toEqual({ primary: id("web-1"), mirrors: [], deletes: [] });
    expect(undo(members, placement, "remove_mirror")).toEqual({ primary: id("web-3"), mirrors: [id("web-2")], deletes: [] });
  });

  it("gives a promoted mirror back to the mirrors", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const placement = row(["web-2"], { primary: "web-2", mirrors: ["web-3"] });
    expect(undo(members, placement, "move")).toEqual({ primary: id("web-1"), mirrors: [id("web-3"), id("web-2")], deletes: [] });
  });

  it("is null when the change was the only one, so Undo discards", () => {
    const members = [member("web-1", "writer"), pooled("web-3")];
    expect(undo(members, row([], { primary: "web-3" }), "move")).toBeNull();
    expect(undo([member("web-2", "writer"), member("web-1", "mirror")], row([], { primary: "web-2", deletes: ["web-1"] }), "delete_copy")).toBeNull();
  });
});

describe("nextPlacementStep", () => {
  const next = (members: Member[], placement: PlacementRow) => nextPlacementStep(changes(members, placement), placement.mirrors ?? []);

  it("drops the promoted mirror once the move ends, and leaves the old primary detached", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror")];
    expect(next(members, row(["web-2"], { primary: "web-2" }))).toEqual({ run: { kind: "move", args: { to: "web-2" } }, mirrors: [] });
  });

  it("keeps the mirrors while a change still has runs after this one", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")];
    expect(next(members, row([], { primary: "web-2", mirrors: ["web-3"] }))).toEqual({
      run: { kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null } },
      mirrors: [],
    });
    const deleted = [member("web-2", "writer"), pooled("web-1"), pooled("web-3")];
    expect(next(deleted, row([], { primary: "web-2", mirrors: ["web-3"] }))).toEqual({ run: { kind: "mirror", args: { to: "web-3" } }, mirrors: [id("web-3")] });
  });

  it("is null once the Servers hold what was staged", () => {
    expect(next([member("web-2", "writer"), member("web-3", "mirror")], row(["web-3"], { primary: "web-2", mirrors: ["web-3"] }))).toBeNull();
  });
});

describe("editPlacement", () => {
  const target = (members: Member[], placement: PlacementRow | null) => {
    const found = targetPlacement(observePlacement(members), placement);
    if (found === null) throw new Error("no primary");
    return found;
  };

  it("promotes the mirror a move targets, and keeps the mirrors of a move elsewhere", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    expect(editPlacement(target(members, row(["web-2"])), { kind: "move", to: id("web-2") })).toEqual({ primary: id("web-2"), mirrors: [], deletes: [] });
    expect(editPlacement(target(members, row(["web-2"])), { kind: "move", to: id("web-3") })).toEqual({ primary: id("web-3"), mirrors: [id("web-2")], deletes: [] });
  });

  it("builds on what is already staged", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const staged = editPlacement(target(members, row(["web-2"], { primary: "web-3", mirrors: ["web-2"] })), { kind: "remove_mirror", server: id("web-2") });
    expect(staged).toEqual({ primary: id("web-3"), mirrors: [], deletes: [] });
  });

  it("stages the mirror as primary when the primary's Server is silent", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    const staged = editPlacement(target(members, row(["web-2"])), { kind: "move", to: id("web-2") });
    expect(staged).toEqual({ primary: id("web-2"), mirrors: [], deletes: [] });
    expect(steps(members, row(["web-2"], { primary: "web-2" }))).toEqual([{ kind: "restore", args: { from: "web-2" } }]);
  });
});

describe("failover", () => {
  it("offers the mirror when the primary stops answering, with the loss time from its newest snapshot", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    expect(failover(observePlacement(members), row(["web-2"]))).toMatchObject({ down: { name: "web-1" }, mirror: { name: "web-2" }, lostAfter: at });
  });

  it("offers nothing once the mirror is staged, or while the primary answers", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    expect(failover(observePlacement(members), row(["web-2"], { primary: "web-2" }))).toBeNull();
    expect(failover(observePlacement([member("web-1", "writer"), member("web-2", "unanswered")]), row([]))).toBeNull();
  });

  it("does not offer a detached copy", () => {
    expect(failover(observePlacement([member("web-1", "unanswered"), member("web-2", "mirror")]), row([]))).toBeNull();
  });
});

describe("placementTargets", () => {
  const servers = ["web-1", "web-2", "web-3"].map(id);

  it("offers a move only to the mirror's Server while a mirror exists", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const targets = placementTargets(volume, members, row(["web-2"]), servers);
    expect(targets.move.map((server) => server.name)).toEqual(["web-2"]);
    expect(targets.mirror).toEqual([]);
  });

  it("offers every other Server once the only other copy is detached, since the Deploy deletes it first", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")];
    const targets = placementTargets(volume, members, row([]), servers);
    expect(targets.move.map((server) => server.name)).toEqual(["web-1", "web-3"]);
    expect(targets.mirror.map((server) => server.name)).toEqual(["web-1", "web-3"]);
  });

  it("offers nothing on a Server that does not accept Services", () => {
    const members = [member("web-1", "writer"), pooled("web-2"), pooled("web-3")];
    expect(placementTargets(volume, members, row([]), [id("web-2")]).move.map((server) => server.name)).toEqual(["web-2"]);
  });
});

describe("storageStep", () => {
  const run = (volumeId: string, message: string | null): VolumeRunView => ({
    id: "r1", volume_id: volumeId, volume_name: "pg-data", kind: "move", args: { to: "web-2" }, orphan: false, state: "running",
    lease: null, message, created_at: "", updated_at: "", finished_at: null,
  });
  const taken = { ...row([]), deploymentId: "dep-1" };

  it("words the run on a Volume the Deploy took, with its message", () => {
    expect(storageStep("dep-1", [taken], [run("v1", null)])).toBe("pg-data · Moving to web-2");
    expect(storageStep("dep-1", [taken], [run("v1", "web-2 did not answer")])).toBe("pg-data · Moving to web-2 · web-2 did not answer");
  });

  it("ignores runs on Volumes another Deploy, or no Deploy, took", () => {
    expect(storageStep("dep-2", [taken], [run("v1", null)])).toBeNull();
    expect(storageStep("dep-1", [taken], [run("v9", null)])).toBeNull();
  });
});

describe("drainPlacement", () => {
  const drain = (members: Member[], placement: PlacementRow | null, accepting = ["web-1", "web-2", "web-3"]) =>
    drainPlacement(volume, members, placement, accepting.map(id), id("web-1"));

  it("moves the primary to its mirror", () => {
    expect(drain([member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")], row(["web-2"])))
      .toEqual({ primary: id("web-2"), mirrors: [], deletes: [] });
  });

  it("moves to the first other Server that accepts Services when there is no mirror", () => {
    expect(drain([member("web-1", "writer"), pooled("web-2"), pooled("web-3")], row([]), ["web-1", "web-3"]))
      .toEqual({ primary: id("web-3"), mirrors: [], deletes: [] });
  });

  it("leaves a Volume whose primary is elsewhere, or that nowhere takes", () => {
    expect(drain([member("web-2", "writer"), member("web-1", "mirror")], row(["web-1"]))).toBeNull();
    expect(drain([member("web-1", "writer"), pooled("web-2")], row([]), ["web-1"])).toBeNull();
  });
});

describe("stagedOffServer", () => {
  const copy = (server: string, volumeId: string, role: "writer" | "slot") => ({ machineId: id(server), name: `shop-production_vol-${volumeId}`, role });

  it("counts staged moves of primaries on the Server, and nothing staged elsewhere", () => {
    const rows = [
      { ...row([], { primary: "web-2" }), volumeId: "v1" },
      { ...row([], { primary: "web-1" }), volumeId: "v2" },
      { ...row([], { primary: "web-3" }), volumeId: "v3" },
      { ...row([]), volumeId: "v4" },
    ];
    const copies = [copy("web-1", "v1", "writer"), copy("web-1", "v2", "writer"), copy("web-2", "v3", "writer"), copy("web-1", "v3", "slot"), copy("web-1", "v4", "writer")];
    expect(stagedOffServer(id("web-1"), rows, copies)).toBe(1);
  });
});
