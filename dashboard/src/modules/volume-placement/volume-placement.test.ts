import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import {
  canvasMirrors,
  detachedCopies,
  drainPlacement,
  editPlacement,
  failover,
  nextPlacementStep,
  observePlacement,
  placementChanges,
  placementSteps,
  placementTargets,
  placementForm,
  mirroredVolumes,
  mirrorPanel,
  mirrorResourceId,
  parseMirrorResourceId,
  mergeRestoreGroups,
  placementGroup,
  restorePoints,
  stagedOffServer,
  storageStep,
  targetPlacement,
  undoPlacement,
  type PlacementItem,
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
  .map(({ kind, name, currentValue, newValue, detail, loss }) => ({ kind, name, currentValue, newValue, detail, loss }));
const primaryMove = (from: string, to: string, loss?: string) =>
  ({ kind: "update", name: "Primary", currentValue: from, newValue: to, detail: undefined, loss });
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

  it("counts a mirror its answering Server no longer holds as removed, and still removes one on a silent or missing Server", () => {
    expect(changes([member("web-1", "writer"), pooled("web-2")], row(["web-2"], { primary: "web-1" }))).toEqual([]);
    expect(steps([member("web-1", "writer"), member("web-2", "unanswered")], row(["web-2"], { primary: "web-1" }))).toEqual([
      { kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null } },
    ]);
    expect(steps([member("web-1", "writer")], row(["web-2"], { primary: "web-1" }))).toEqual([
      { kind: "delete_mirror", args: { slot: id("web-2"), confirmed_name: null } },
    ]);
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

describe("placementForm", () => {
  const form = (members: Member[], placement: PlacementRow | null) => {
    const groups = placementForm(volume, members, placement);
    if (groups === null) throw new Error("no form");
    const items = (list: readonly PlacementItem[]) => list.map(({ server, change }) => `${server.name}:${change ?? "none"}`);
    return { primary: items([groups.primary])[0], mirrors: items(groups.mirrors), detached: items(groups.detached) };
  };

  it("marks nothing when nothing is staged, even while the primary is silent", () => {
    expect(form([member("web-1", "unanswered"), member("web-2", "mirror")], row(["web-2"])))
      .toEqual({ primary: "web-1:none", mirrors: ["web-2:none"], detached: [] });
  });

  it("changes the Primary on a staged move and creates the detached copy it leaves", () => {
    expect(form([member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-3" })))
      .toEqual({ primary: "web-3:update", mirrors: [], detached: ["web-1:add"] });
  });

  it("creates a new mirror and removes the detached copy it deletes first", () => {
    expect(form([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([], { primary: "web-2", mirrors: ["web-3"] })))
      .toEqual({ primary: "web-2:none", mirrors: ["web-3:add"], detached: ["web-1:remove"] });
  });

  it("keeps a removed mirror and a deleted copy in their groups as removes", () => {
    expect(form([member("web-1", "writer"), member("web-2", "mirror")], row(["web-2"], { primary: "web-1" })).mirrors).toEqual(["web-2:remove"]);
    expect(form([member("web-2", "writer"), member("web-1", "mirror")], row([], { primary: "web-2", deletes: ["web-1"] })).detached).toEqual(["web-1:remove"]);
  });

  it("is null when no Server holds the Volume", () => {
    expect(placementForm(volume, [], null)).toBeNull();
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

describe("canvasMirrors", () => {
  const nodes = (members: Member[], placement: PlacementRow | null) =>
    canvasMirrors(volume, members, placement).map(({ volume: owner, server, change, newest }) => ({ owner: owner.id, server: server.name, change, newest }));

  it("draws a live mirror with when it last refreshed", () => {
    expect(nodes([member("web-1", "writer"), mirrorAsOf("web-2", at)], row(["web-2"])))
      .toEqual([{ owner: "v1", server: "web-2", change: null, newest: at }]);
  });

  it("draws a staged mirror as one the Deploy adds", () => {
    expect(nodes([member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-1", mirrors: ["web-3"] })))
      .toEqual([{ owner: "v1", server: "web-3", change: "add", newest: null }]);
  });

  it("keeps a mirror staged for removal, as one the Deploy removes", () => {
    expect(nodes([member("web-1", "writer"), mirrorAsOf("web-2", at)], row(["web-2"], { primary: "web-1" })))
      .toEqual([{ owner: "v1", server: "web-2", change: "remove", newest: at }]);
  });

  it("keeps a mirror staged to become the Primary, as one the Deploy changes", () => {
    expect(nodes([member("web-1", "writer"), mirrorAsOf("web-2", at)], row(["web-2"], { primary: "web-2" })))
      .toEqual([{ owner: "v1", server: "web-2", change: "update", newest: at }]);
  });

  it("draws none without mirrors, and leaves detached copies to the drawer", () => {
    expect(nodes([member("web-1", "writer")], row([]))).toEqual([]);
    expect(nodes([member("web-2", "writer"), member("web-1", "mirror")], row([]))).toEqual([]);
    expect(nodes([], null)).toEqual([]);
  });
});

describe("mirrorResourceId", () => {
  it("addresses a mirror by its Volume and Server, and reads back only a mirror's address", () => {
    expect(parseMirrorResourceId(mirrorResourceId("v1", id("web-2")))).toEqual({ volumeId: "v1", server: id("web-2") });
    expect(parseMirrorResourceId("79997413-396c-471e-a1ad-e89164cf4e9c")).toBeNull();
    expect(parseMirrorResourceId("v1~")).toBeNull();
    expect(parseMirrorResourceId("~web-2")).toBeNull();
  });
});

describe("mirrorPanel", () => {
  const servers = ["web-1", "web-2", "web-3"].map(id);
  const live = [member("web-1", "writer"), mirrorAsOf("web-2", at), pooled("web-3")];

  it("offers a live mirror both actions, each staging what the volume drawer stages", () => {
    const panel = mirrorPanel(volume, live, row(["web-2"]), id("web-2"), servers);
    expect(panel).toMatchObject({ server: { name: "web-2" }, change: null, newest: at, applied: [id("web-2")] });
    expect(panel?.promote).toEqual(editPlacement({ primary: id("web-1"), mirrors: [id("web-2")], deletes: [] }, { kind: "move", to: id("web-2") }));
    expect(panel?.promote).toEqual({ primary: id("web-2"), mirrors: [], deletes: [] });
    expect(panel?.remove).toEqual({ primary: id("web-1"), mirrors: [], deletes: [] });
  });

  it("offers nothing on a staged mirror: Details discards it", () => {
    const added = mirrorPanel(volume, [member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-1", mirrors: ["web-3"] }), id("web-3"), servers);
    expect(added).toMatchObject({ change: "add", newest: null, promote: null, remove: null });
    expect(mirrorPanel(volume, live, row(["web-2"], { primary: "web-2" }), id("web-2"), servers))
      .toMatchObject({ change: "update", promote: null, remove: null });
  });

  it("offers nothing while a Deploy runs the placement", () => {
    const running: PlacementRow = { ...row(["web-2"]), deploymentId: "d1" };
    expect(mirrorPanel(volume, live, running, id("web-2"), servers)).toMatchObject({ promote: null, remove: null });
  });

  it("does not make primary a mirror on a Server that does not accept Services", () => {
    const panel = mirrorPanel(volume, live, row(["web-2"]), id("web-2"), [id("web-1")]);
    expect(panel?.promote).toBeNull();
    expect(panel?.remove).toEqual({ primary: id("web-1"), mirrors: [], deletes: [] });
  });

  it("finds no mirror on a Server without one", () => {
    expect(mirrorPanel(volume, live, row(["web-2"]), id("web-3"), servers)).toBeNull();
    expect(mirrorPanel(volume, live, row(["web-2"]), id("web-1"), servers)).toBeNull();
  });
});

describe("mirroredVolumes", () => {
  const placement = (volumeId: string, mirrors: string[] | null, staged: boolean, environmentId = "e1"): PlacementRow =>
    ({ ...row(mirrors, staged ? { primary: "web-1" } : null), volumeId, environmentId });

  it("asks the Servers only about Volumes here that have, or will have, a mirror", () => {
    const rows = [placement("listed", ["web-2"], false), placement("staged", [], true), placement("bare", [], false), placement("elsewhere", ["web-2"], false, "e2")];
    const copies = [
      { machineId: "web-2", name: "ns_vol-cli-made", role: "slot" as const },
      { machineId: "web-1", name: "ns_vol-none", role: "writer" as const },
    ];
    expect(mirroredVolumes(["listed", "staged", "bare", "elsewhere", "cli-made", "none"], rows, "e1", copies))
      .toEqual(["listed", "staged", "cli-made"]);
  });
});

describe("restore into a new Volume", () => {
  const point = { guid: "11", name: "ployz-1", at: at.getTime() / 1000 };
  const seeded: PlacementRow = {
    ...row([], { primary: "web-1" }),
    staged: { primary: id("web-1"), mirrors: [], deletes: [], seed: { sourceVolumeId: "src", sourceName: "data", dockerVolume: "ns_vol-v1", point } },
  };

  it("copies the restore point on the source's primary until a root of the new Volume answers", () => {
    expect(nextPlacementStep(changes([], seeded), [])).toEqual({
      run: { kind: "seed", args: { into: { volumeId: "v1", dockerVolume: "ns_vol-v1", name: "pg-data" }, guid: "11", name: "ployz-1" } },
      mirrors: [],
    });
    expect(changes([member("web-1", "writer")], seeded)).toEqual([]);
  });

  it("reviews as a created Volume that says what it leaves out, and has no inline undo", () => {
    const [change] = changes([], seeded);
    if (change === undefined) throw new Error("fixture");
    const group = placementGroup(volume, [change]);
    expect(group.lifecycle).toBe("create");
    expect(group.rows).toMatchObject([{
      kind: "add", name: "From", currentValue: "", newValue: "data · 10:36",
      loss: "Changes after 10:36 are not in pg-data", detail: "Volume data, not database point-in-time.",
    }]);
    expect(undoPlacement(volume, [], seeded, change)).toBeNull();
  });

  it("lists each mirror's newest restore point once, newest first, from Servers that answer", () => {
    const points = restorePoints([
      member("web-1", "writer"), member("web-2", "mirror"), mirrorAsOf("web-3", at), member("web-4", "mirror"), member("web-5", "unanswered"),
    ]);
    expect(points.map(({ guid, server }) => ({ guid, server: server.name }))).toEqual([{ guid: "12", server: "web-3" }, { guid: "11", server: "web-2" }]);
  });
});

describe("mergeRestoreGroups", () => {
  const group = (owner: "store" | "placement", nodeId: string, lifecycle: "create" | "update", rows: number): ChangeGroup => ({
    owner, nodeType: "volume", nodeId, nodeName: nodeId, discardPath: nodeId, row: null, lifecycle, restarts: [], canDiscard: true,
    changeCount: rows,
    rows: Array.from({ length: rows }, (_, index) => ({
      changeKey: `${owner}:${index}`, path: String(index), row: null, kind: "add" as const, label: owner, name: owner, variable: false,
      currentValue: "", newValue: "", canDiscard: true,
    })),
  });

  it("shows a restored Volume once, discarded as a whole through its placement", () => {
    const merged = mergeRestoreGroups(
      [group("store", "new", "create", 1), group("store", "web", "update", 1)],
      [group("placement", "new", "create", 1), group("placement", "other", "update", 2)],
    );
    expect(merged.map(({ owner, nodeId, changeCount }) => ({ owner, nodeId, changeCount }))).toEqual([
      { owner: "placement", nodeId: "new", changeCount: 2 },
      { owner: "store", nodeId: "web", changeCount: 1 },
      { owner: "placement", nodeId: "other", changeCount: 2 },
    ]);
    expect(merged[0]?.rows.map((row) => row.canDiscard)).toEqual([true, false]);
  });

  it("keeps a restore the Store has not listed yet", () => {
    expect(mergeRestoreGroups([], [group("placement", "new", "create", 1)]).map(({ nodeId }) => nodeId)).toEqual(["new"]);
  });
});
