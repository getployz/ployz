// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { volumeCopies } from "#/modules/volume-run/volume-copies";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { member, run } from "#/modules/volume-run/volume-run.test-fixture";
import { VolumeCopiesPanel } from "./StoreVolumeCopies";

const pooled = (server: string) => member(server, "empty", { pool: true });

function panel(members: Member[], runs: VolumeRunView[] = []) {
  const request = vi.fn();
  const machines = members.map((entry) => ({ id: entry.machine.id, acceptsServices: true }));
  const view = volumeCopies({ volume: { name: "data" }, members, machines, runs });
  render(<VolumeCopiesPanel view={view} runs={runs} observed pending={false} request={request} />);
  return request;
}
const copiesShown = () => screen.getAllByRole("listitem").map((item) => item.textContent);
const buttons = () => screen.queryAllByRole("button").filter((button) => button.getAttribute("aria-label") === null).map((button) => button.textContent);

describe("VolumeCopiesPanel", () => {
  afterEach(cleanup);

  it("offers Mirror and Move for a writer alone, and starts a Move to the only other Server", () => {
    const request = panel([member("web-1", "writer"), pooled("web-2")]);
    expect(copiesShown()).toEqual(["dataWriteron web-1"]);
    expect(buttons()).toEqual(["Mirror", "Move"]);
    fireEvent.click(screen.getByRole("button", { name: "Move" }));
    expect(request).toHaveBeenCalledWith({ kind: "move", args: { to: "web-2" } });
  });

  it("shows the mirror as data-<server> and offers Sync", () => {
    const request = panel([member("web-1", "writer"), member("web-2", "mirror")]);
    expect(copiesShown()).toEqual(["dataWriteron web-1", "data-web-2Mirroron web-2"]);
    expect(buttons()).toEqual(["Sync", "Move"]);
    fireEvent.click(screen.getByRole("button", { name: "Sync" }));
    expect(request).toHaveBeenCalledWith({ kind: "sync", args: { full: false } });
  });

  it("shows a Move in progress and offers nothing", () => {
    panel([member("web-1", "switching"), member("web-2", "final")], [run("move", { to: "web-2" }, "running")]);
    expect(copiesShown()).toEqual(["data-web-1Movingon web-1", "data-web-2Mirroron web-2"]);
    expect(screen.getByText(/Move to web-2 · running since/)).toBeTruthy();
    expect(buttons()).toEqual([]);
    expect(screen.getByText("Activity").parentElement?.textContent).toContain("Move to web-2Running");
  });

  it("offers Restore from the mirror once the writer's Server is removed", () => {
    const request = panel([member("web-2", "mirror")]);
    expect(copiesShown()).toEqual(["data-web-2Mirroron web-2"]);
    expect(buttons()).toEqual(["Restore"]);
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    expect(request).toHaveBeenCalledWith({ kind: "restore", args: { from: "web-2" } });
  });

  it("says a Server that does not answer holds runs back", () => {
    panel([member("web-1", "unanswered"), member("web-2", "mirror")]);
    expect(screen.getByRole("note").textContent).toContain("web-1 did not answer, so its copy is unknown");
    expect(buttons()).toEqual([]);
  });

  it("marks the root with the lower record as old, and says the next run seals it", () => {
    panel([member("web-1", "writer", { lease: 7 }), member("web-2", "writer", { lease: 8 })]);
    expect(copiesShown()).toEqual(["dataWriteron web-2", "data-web-1Oldon web-1"]);
    expect(screen.getByRole("note").textContent).toContain("data-web-1 came back after another server took over");
  });

  it("warns, and offers nothing, for two writers whose records tie", () => {
    panel([member("web-1", "writer", { lease: 8 }), member("web-2", "writer", { lease: 8 })]);
    expect(screen.getByRole("note").textContent).toContain("Two servers each hold this volume");
    expect(buttons()).toEqual([]);
  });
});
