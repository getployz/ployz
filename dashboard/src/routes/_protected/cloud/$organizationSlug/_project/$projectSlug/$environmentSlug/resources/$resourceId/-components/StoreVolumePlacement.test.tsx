// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { COPIES, member, run } from "#/modules/volume-run/volume-run.test-fixture";
import { VolumePlacementPanel } from "./StoreVolumePlacement";

const id = (server: string) => server.padEnd(32, "0") as MachineId;
const pooled = (server: string) => member(server, "empty", { pool: true });
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null, deploymentId: string | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: (staged.deletes ?? []).map(id) },
  deploymentId,
});
const mirrorAsOf = (server: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: "s", guid: "12", created_unix_seconds: when.getTime() / 1000 } } } };
};

function panel(members: Member[], placement: PlacementRow | null, running: VolumeRunView | null = null) {
  const stage = vi.fn();
  render(<VolumePlacementPanel volume={{ id: "v1", name: "data" }} members={members} row={placement} running={running}
    accepting={members.map((entry) => entry.machine.id)} disabled={false} stage={stage} />);
  return stage;
}
const rows = () => screen.getAllByRole("listitem").map((item) => item.textContent);
const staged = () => screen.getAllByRole("listitem").map((item) => item.hasAttribute("data-changed"));
const struck = (name: string) => screen.getByText(name).className.includes("line-through");
async function choose(menu: string, item: string) {
  fireEvent.click(screen.getByRole("button", { name: menu }));
  fireEvent.click(await screen.findByRole("menuitem", { name: item }));
}

describe("VolumePlacementPanel", () => {
  afterEach(cleanup);

  it("shows the Primary and its mirror with no badge or staged treatment, and stages a move that promotes the mirror", async () => {
    const stage = panel([member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")], row(["web-2"]));
    expect(rows()).toEqual(["dataweb-1", "data-web-2web-2 · Mirror"]);
    expect(staged()).toEqual([false, false]);
    expect(screen.queryByRole("button", { name: "Add mirror" })).toBeNull();
    await choose("Actions for data", "Move to web-2");
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("shows a staged move in pink on the new Server and leaves the old copy detached, with no Was or Undo", () => {
    panel([member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-3" }));
    expect(rows()).toEqual(["dataweb-3", "data-oldweb-1 · Detached"]);
    expect(staged()).toEqual([true, true]);
    expect(screen.queryByText(/staged|was/i)).toBeNull();
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });

  it("stages a mirror, and staged rows offer no actions", async () => {
    const add = panel([member("web-1", "writer"), pooled("web-3")], null);
    await choose("Add mirror", "web-3");
    expect(add).toHaveBeenCalledWith({ primary: id("web-1"), mirrors: [id("web-3")], deletes: [] }, []);
    cleanup();
    panel([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([], { primary: "web-2", mirrors: ["web-3"], deletes: ["web-1"] }));
    expect(rows()).toEqual(["dataweb-2", "data-web-3web-3 · Mirror", "data-oldweb-1 · Detached"]);
    expect(screen.queryByRole("button", { name: "Actions for data-web-3" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Actions for data-old" })).toBeNull();
  });

  it("strikes a removed mirror and keeps it in the list until Deploy", () => {
    panel([member("web-1", "writer"), member("web-2", "mirror")], row(["web-2"], { primary: "web-1" }));
    expect(rows()).toEqual(["dataweb-1", "data-web-2web-2 · Mirror"]);
    expect(staged()).toEqual([false, true]);
    expect(struck("data-web-2")).toBe(true);
  });

  it("deletes a detached copy from its menu", async () => {
    const stage = panel([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([]));
    expect(rows()).toEqual(["dataweb-2", "data-oldweb-1 · Detached"]);
    expect(screen.queryByRole("note")).toBeNull();
    await choose("Actions for data-old", "Delete");
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [id("web-1")] }, []);
  });

  it("offers the mirror when the Primary's Server stops answering, without marking the silent Primary staged", () => {
    const stage = panel([member("web-1", "unanswered"), mirrorAsOf("web-2", new Date(2026, 9, 10, 10, 36))], row(["web-2"]));
    expect(screen.getByRole("alert").textContent).toBe("web-1 is not answeringChanges after 10:36 lostUse mirror");
    expect(staged()).toEqual([false, false]);
    fireEvent.click(screen.getByRole("button", { name: "Use mirror" }));
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("shows the run under way, and drops the staged treatment and edits while a Deploy runs the move", () => {
    panel([member("web-1", "writer"), pooled("web-3"), pooled("web-4")], row([], { primary: "web-3" }, "dep-1"));
    expect(screen.getByRole("status").textContent).toBe("Deploying");
    expect(rows()).toEqual(["dataweb-3", "data-oldweb-1 · Detached"]);
    expect(staged()).toEqual([false, false]);
    expect(screen.getByRole("button", { name: "Add mirror" }).hasAttribute("disabled")).toBe(true);
    cleanup();
    panel([member("web-1", "writer"), member("web-3", "mirror")], row(["web-3"]), run("move", { to: "web-3" }, "running"));
    expect(screen.getByRole("status").textContent).toBe("Moving to web-3");
    expect(screen.getByRole("button", { name: "Actions for data" }).hasAttribute("disabled")).toBe(false);
  });
});
