// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { volumeCopies } from "#/modules/volume-run/volume-copies";
import type { VolumeRunView } from "#/modules/volume-run/volume-run";
import { VolumeCopiesPanel } from "./StoreVolumeCopies";

const server = (id: string, membership = "up") => ({ id, name: id, membership, storage: "pool" as const, acceptsServices: true });
const copy = (machineId: string, role: RuntimeVolumeCopy["role"]): RuntimeVolumeCopy => ({ machineId, name: "app-production_vol-v1", role });
const run = (kind: VolumeRunView["kind"], args: VolumeRunView["args"], state: VolumeRunView["state"]): VolumeRunView => ({
  id: `${kind}-${state}`, volume_id: "v1", volume_name: "data", kind, args, orphan: false, state, lease: 1, message: null,
  created_at: "2026-10-09T00:00:00.000Z", updated_at: "2026-10-09T00:00:00.000Z", finished_at: null,
});

function panel(copies: RuntimeVolumeCopy[], runs: VolumeRunView[] = [], machines = [server("web-1"), server("web-2")]) {
  const request = vi.fn();
  const view = volumeCopies({ volume: { id: "v1", name: "data" }, copies, machines, runs });
  render(<VolumeCopiesPanel view={view} runs={runs} observed pending={false} request={request} />);
  return request;
}
const copiesShown = () => screen.getAllByRole("listitem").map((item) => item.textContent);
const buttons = () => screen.queryAllByRole("button").filter((button) => button.getAttribute("aria-label") === null).map((button) => button.textContent);

describe("VolumeCopiesPanel", () => {
  afterEach(cleanup);

  it("offers Mirror and Move for a writer alone, and starts a Move to the only other Server", () => {
    const request = panel([copy("web-1", "writer")]);
    expect(copiesShown()).toEqual(["dataWriteron web-1"]);
    expect(buttons()).toEqual(["Mirror", "Move"]);
    fireEvent.click(screen.getByRole("button", { name: "Move" }));
    expect(request).toHaveBeenCalledWith({ kind: "move", args: { to: "web-2" } });
  });

  it("shows the mirror as data-<server> and offers Sync", () => {
    const request = panel([copy("web-1", "writer"), copy("web-2", "slot")]);
    expect(copiesShown()).toEqual(["dataWriteron web-1", "data-web-2Mirroron web-2"]);
    expect(buttons()).toEqual(["Sync", "Move"]);
    fireEvent.click(screen.getByRole("button", { name: "Sync" }));
    expect(request).toHaveBeenCalledWith({ kind: "sync", args: { full: false } });
  });

  it("shows a Move in progress and offers nothing", () => {
    panel([copy("web-1", "writer"), copy("web-2", "switching")], [run("move", { to: "web-2" }, "running")]);
    expect(copiesShown()).toEqual(["dataWriteron web-1", "data-web-2Movingon web-2"]);
    expect(screen.getByText(/Move to web-2 · running since/)).toBeTruthy();
    expect(buttons()).toEqual([]);
    expect(screen.getByText("Activity").parentElement?.textContent).toContain("Move to web-2Running");
  });

  it("offers Restore from the mirror once the writer's Server is gone", () => {
    const request = panel([copy("web-2", "slot")], [], [server("web-1", "down"), server("web-2")]);
    expect(copiesShown()).toEqual(["data-web-2Mirroron web-2"]);
    expect(buttons()).toEqual(["Restore"]);
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    expect(request).toHaveBeenCalledWith({ kind: "restore", args: { from: "web-2" } });
  });

  it("marks the copy a Restore left behind as old, and says the next run seals it", () => {
    panel([copy("web-1", "writer"), copy("web-2", "writer")], [run("restore", { from: "web-2" }, "done")]);
    expect(copiesShown()).toEqual(["dataWriteron web-2", "data-web-1Oldon web-1"]);
    expect(screen.getByRole("note").textContent).toContain("data-web-1 came back after another server took over");
  });

  it("says nothing more of an old copy its Server already made read-only", () => {
    panel([copy("web-2", "writer"), copy("web-1", "old")]);
    expect(copiesShown()).toEqual(["dataWriteron web-2", "data-web-1Oldon web-1"]);
    expect(screen.queryByRole("note")).toBeNull();
  });

  it("warns, and offers nothing, for two writers no run tells apart", () => {
    panel([copy("web-1", "writer"), copy("web-2", "writer")]);
    expect(screen.getByRole("note").textContent).toContain("Two servers each hold this volume");
    expect(buttons()).toEqual([]);
  });
});
