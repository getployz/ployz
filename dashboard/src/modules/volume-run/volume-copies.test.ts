import { describe, expect, it } from "vitest";
import type { RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { runText, volumeCopies } from "./volume-copies";
import type { VolumeRunView } from "./volume-run";

const volume = { id: "v1", name: "data" };
const NAME = "app-production_vol-v1";
const server = (id: string, membership = "up") => ({ id, name: id, membership, storage: "pool" as const, acceptsServices: true });
const copy = (machineId: string, role: RuntimeVolumeCopy["role"], name = NAME): RuntimeVolumeCopy => ({ machineId, name, role });
const run = (kind: VolumeRunView["kind"], args: VolumeRunView["args"], state: VolumeRunView["state"]): VolumeRunView => ({
  id: `${kind}-${state}`, volume_id: "v1", volume_name: "data", kind, args, orphan: false, state, lease: 1, message: null,
  created_at: "2026-10-09T00:00:00.000Z", updated_at: "2026-10-09T00:00:00.000Z", finished_at: null,
});
const machines = [server("web-1"), server("web-2"), server("web-3")];

describe("volumeCopies", () => {
  it("offers a mirror and a move for a writer alone, and only this Volume's copies count", () => {
    const view = volumeCopies({ volume, machines, runs: [], copies: [copy("web-1", "writer"), copy("web-2", "writer", "app-production_vol-v2")] });
    expect(view.copies).toEqual([{ server: "web-1", machineId: "web-1", label: "data", role: "writer", online: true }]);
    expect(view.offers).toEqual({ mirror: { servers: ["web-2", "web-3"] }, sync: null, move: { servers: ["web-2", "web-3"] }, release: null, restore: null });
  });

  it("names the mirror data-<server> and offers Sync instead of a second mirror", () => {
    const view = volumeCopies({ volume, machines, runs: [], copies: [copy("web-2", "slot"), copy("web-1", "writer")] });
    expect(view.copies.map((entry) => [entry.label, entry.role])).toEqual([["data", "writer"], ["data-web-2", "mirror"]]);
    expect(view.offers.mirror).toBeNull();
    expect(view.offers.sync).toEqual({ servers: [] });
    // A move may land on the mirror's Server: it starts from the mirror.
    expect(view.offers.move).toEqual({ servers: ["web-2", "web-3"] });
  });

  it("offers nothing while a run is in progress, and shows it", () => {
    const moving = run("move", { to: "web-2" }, "running");
    const view = volumeCopies({ volume, machines, runs: [moving], copies: [copy("web-1", "writer"), copy("web-2", "switching")] });
    expect(view.active).toBe(moving);
    expect(view.copies.map((entry) => entry.role)).toEqual(["writer", "moving"]);
    expect(Object.values(view.offers).every((offer) => offer === null)).toBe(true);
  });

  it("offers Restore from a mirror once no Server holds the writer", () => {
    const view = volumeCopies({ volume, machines: [server("web-1", "down"), server("web-2")], runs: [], copies: [copy("web-2", "slot")] });
    expect(view.offers).toEqual({ mirror: null, sync: null, move: null, release: null, restore: { servers: ["web-2"] } });
  });

  it("offers no Restore while the writer's Server still reports it, even offline", () => {
    const view = volumeCopies({ volume, machines: [server("web-1", "down"), server("web-2")], runs: [], copies: [copy("web-1", "writer"), copy("web-2", "slot")] });
    expect(view.offers.restore).toBeNull();
    expect(view.offers.move).toBeNull();
  });

  it("names a returning writer old when the last Restore made another the writer", () => {
    const view = volumeCopies({
      volume, machines,
      runs: [run("restore", { from: "web-2" }, "done"), run("mirror", { to: "web-2" }, "done")],
      copies: [copy("web-1", "writer"), copy("web-2", "writer")],
    });
    expect(view.copies.map((entry) => [entry.server, entry.label, entry.role])).toEqual([["web-2", "data", "writer"], ["web-1", "data-web-1", "old"]]);
    expect(view.twoWriters).toBe(false);
    expect(view.sealing).toBe("data-web-1");
    expect(view.offers.move).toEqual({ servers: ["web-1", "web-3"] });
  });

  it("shows a demoted copy as old", () => {
    const view = volumeCopies({ volume, machines, runs: [], copies: [copy("web-2", "writer"), copy("web-1", "old")] });
    expect(view.copies.map((entry) => [entry.label, entry.role])).toEqual([["data", "writer"], ["data-web-1", "old"]]);
    expect(view.offers.mirror).toEqual({ servers: ["web-3"] });
    expect(view.sealing).toBeNull();
  });

  it("offers nothing for two writers no run tells apart", () => {
    const view = volumeCopies({ volume, machines, runs: [], copies: [copy("web-1", "writer"), copy("web-2", "writer")] });
    expect(view.twoWriters).toBe(true);
    expect(Object.values(view.offers).every((offer) => offer === null)).toBe(true);
  });

  it("offers Release after a move that stopped", () => {
    const view = volumeCopies({ volume, machines, runs: [run("move", { to: "web-2" }, "failed")], copies: [copy("web-1", "writer")] });
    expect(view.offers.release).toEqual({ servers: [] });
    expect(volumeCopies({ volume, machines, runs: [run("move", { to: "web-2" }, "done")], copies: [copy("web-2", "writer")] }).offers.release).toBeNull();
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
