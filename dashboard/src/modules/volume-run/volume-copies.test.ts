import { describe, expect, it } from "vitest";
import type { RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { runText, trayCopy, volumeCopies } from "./volume-copies";
import type { Member, VolumeRunView } from "./volume-run";
import { member, run } from "./volume-run.test-fixture";

const volume = { id: "v1", name: "data" };
const pooled = (server: string) => member(server, "empty", { pool: true });
const takers = (members: readonly Member[]) => members.map((entry) => ({ id: entry.machine.id, acceptsServices: true }));
const view = (members: Member[], runs: VolumeRunView[] = [], machines = takers(members)) => volumeCopies({ volume, members, machines, runs });
const offered = (members: Member[], runs: VolumeRunView[] = []) =>
  Object.fromEntries(Object.entries(view(members, runs).offers).flatMap(([kind, offer]) => (offer === null ? [] : [[kind, offer.servers]])));

describe("volumeCopies", () => {
  it("offers a mirror and a move for a writer alone", () => {
    const shown = view([member("web-1", "writer", { lease: 3 }), pooled("web-2"), pooled("web-3")]);
    expect(shown.copies).toEqual([{ server: "web-1", machineId: member("web-1", "writer").machine.id, label: "data", role: "writer" }]);
    expect(offered([member("web-1", "writer"), pooled("web-2"), pooled("web-3")])).toEqual({ mirror: ["web-2", "web-3"], move: ["web-2", "web-3"] });
  });

  it("offers Move only to the mirror's Server, since Cloud refuses a second mirror", () => {
    const members = [member("a", "writer"), member("b", "mirror"), pooled("c")];
    expect(view(members).copies.map((entry) => [entry.label, entry.role])).toEqual([["data", "writer"], ["data-b", "mirror"]]);
    expect(offered(members)).toEqual({ sync: [], move: ["b"] });
  });

  it("names Mirror and Move targets only among Servers that take Services", () => {
    const [b, c] = [pooled("b"), pooled("c")];
    const members = [member("a", "writer"), b, c];
    const machines = [{ id: b.machine.id, acceptsServices: true }, { id: c.machine.id, acceptsServices: false }];
    expect(view(members, [], machines).offers).toMatchObject({ mirror: { servers: ["b"] }, move: { servers: ["b"] } });
  });

  it("offers nothing while a run is in progress, and shows it", () => {
    const moving = run("move", { to: "b" }, "running");
    const shown = view([member("a", "switching"), member("b", "final")], [moving]);
    expect(shown.active).toBe(moving);
    expect(shown.copies.map((entry) => entry.role)).toEqual(["moving", "mirror"]);
    expect(Object.values(shown.offers).every((offer) => offer === null)).toBe(true);
  });

  it("after a failed Move past handover, offers the Move to its target and neither Release nor Restore", () => {
    const members = [member("a", "handed", { lease: 8 }), member("b", "stale", { lease: 8 }), pooled("c")];
    expect(view(members).copies.map((entry) => [entry.server, entry.role])).toEqual([["a", "moving"], ["b", "moving"]]);
    expect(offered(members, [run("move", { to: "b" }, "failed")])).toEqual({ move: ["b"] });
  });

  it("after a Move stopped before handover, offers Release and the Move to its target, and no Restore", () => {
    const members = [member("a", "switching"), member("b", "final"), pooled("c")];
    expect(offered(members, [run("move", { to: "b" }, "failed")])).toEqual({ move: ["b"], release: [] });
  });

  it("offers Restore from the copy left once the writer's Server is removed", () => {
    expect(offered([member("b", "mirror")])).toEqual({ restore: ["b"] });
  });

  it("offers nothing, and names the Server, while one does not answer", () => {
    const shown = view([member("a", "unanswered"), member("b", "mirror")]);
    expect(shown.unanswered).toEqual(["a"]);
    expect(Object.values(shown.offers).every((offer) => offer === null)).toBe(true);
  });

  it("names the root with the lower record old without any run history, and offers what the run after its demote plans", () => {
    const shown = view([member("a", "writer", { lease: 7 }), member("b", "writer", { lease: 8 }), pooled("c")]);
    expect(shown.copies.map((entry) => [entry.server, entry.label, entry.role])).toEqual([["b", "data", "writer"], ["a", "data-a", "old"]]);
    expect(shown.twoWriters).toBe(false);
    expect(shown.sealing).toBe("data-a");
    expect(shown.offers).toMatchObject({ mirror: { servers: ["c"] }, move: { servers: ["c"] } });
  });

  it("keeps a copy an interrupted demote left read-only old, even with the higher record", () => {
    const shown = view([member("a", "promoted", { lease: 9 }), member("b", "writer", { lease: 8 }), pooled("c")]);
    expect(shown.copies.map((entry) => [entry.server, entry.role])).toEqual([["b", "writer"], ["a", "old"]]);
    expect(shown.sealing).toBe("data-a");
  });

  it("promises no seal for tied writers, whatever the run history says", () => {
    const shown = view([member("a", "writer", { lease: 8 }), member("b", "writer", { lease: 8 })], [run("restore", { from: "b" }, "done")]);
    expect(shown.twoWriters).toBe(true);
    expect(shown.sealing).toBeNull();
    expect(shown.copies.map((entry) => entry.role)).toEqual(["writer", "writer"]);
    expect(Object.values(shown.offers).every((offer) => offer === null)).toBe(true);
  });

  it("offers no Release when no Move left anything to release", () => {
    expect(offered([member("a", "writer"), pooled("b")], [run("move", { to: "b" }, "failed")])).toEqual({ mirror: ["b"], move: ["b"] });
  });
});

describe("trayCopy", () => {
  const NAME = "app-production_vol-v1";
  const machines = [{ id: "web-1", name: "web-1" }, { id: "web-2", name: "web-2" }];
  const copy = (machineId: string, role: RuntimeVolumeCopy["role"], name = NAME): RuntimeVolumeCopy => ({ machineId, name, role });
  const tray = (copies: RuntimeVolumeCopy[], active: VolumeRunView | null = null) => trayCopy({ volume, copies, machines, active });

  it("names the Move's target from its run, not the switching source", () => {
    expect(tray([copy("web-1", "switching"), copy("web-2", "slot")], run("move", { to: "web-2" }, "running"))).toEqual({ kind: "moving", text: "Moving to web-2" });
  });

  it("names no target for a switching copy with no Move running", () => {
    expect(tray([copy("web-1", "switching"), copy("web-2", "slot")])).toEqual({ kind: "moving", text: "Moving" });
  });

  it("names the mirror, and only this Volume's copies count", () => {
    expect(tray([copy("web-1", "writer"), copy("web-2", "slot")])).toEqual({ kind: "mirror", label: "data-web-2" });
    expect(tray([copy("web-1", "writer"), copy("web-2", "slot", "app-production_vol-v2")])).toBeNull();
  });
});

describe("runText", () => {
  it("says each run in the CLI's words", () => {
    expect(runText(run("move", { to: "web-2" }, "running"))).toEqual({ what: "Move to web-2", state: "Running" });
    expect(runText(run("sync", { full: true }, "done"))).toEqual({ what: "Full sync", state: "Done" });
    expect(runText(run("delete_mirror", { slot: "web-2", confirmed_name: null }, "lost"))).toEqual({ what: "Delete mirror data-web-2", state: "Lost" });
    expect(runText(run("restore", { from: "web-2" }, "requested"))).toEqual({ what: "Restore from web-2", state: "Starting" });
  });
});
