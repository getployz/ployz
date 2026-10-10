// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import { VolumePlacementPanel } from "./StoreVolumePlacement";

const id = (server: string) => server.padEnd(32, "0") as MachineId;
const pooled = (server: string) => member(server, "empty", { pool: true });
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[] } | null = null, deploymentId: string | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: [] },
  deploymentId,
});
const mirrorAsOf = (server: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: "s", guid: "12", created_unix_seconds: when.getTime() / 1000 } } } };
};

function panel(members: Member[], placement: PlacementRow | null) {
  const stage = vi.fn();
  render(<VolumePlacementPanel volume={{ id: "v1", name: "data" }} members={members} row={placement} running={null}
    accepting={members.map((entry) => entry.machine.id)} disabled={false} stage={stage} />);
  return stage;
}
const rows = () => screen.getAllByRole("listitem").map((item) => item.textContent);
async function choose(menu: string, item: string) {
  fireEvent.click(screen.getByRole("button", { name: menu }));
  fireEvent.click(await screen.findByRole("menuitem", { name: item }));
}

describe("VolumePlacementPanel", () => {
  afterEach(cleanup);

  it("shows the Primary and its one mirror, and stages a move that promotes the mirror", async () => {
    const stage = panel([member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")], row(["web-2"]));
    expect(rows()).toEqual(["dataPrimaryweb-1", "data-web-2Mirrorweb-2 · Refreshes on every deploy and every 15 minutes"]);
    expect(screen.getByText("One mirror for now")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Add mirror" })).toBeNull();
    await choose("data options", "Move to web-2");
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("stages a mirror on a Server the planner accepts, with no warning before any move", async () => {
    const stage = panel([member("web-1", "writer"), pooled("web-3")], null);
    expect(screen.queryByRole("note")).toBeNull();
    await choose("Add mirror", "web-3");
    expect(stage).toHaveBeenCalledWith({ primary: id("web-1"), mirrors: [id("web-3")], deletes: [] }, []);
  });

  it("keeps the old copy detached after a move, warns there is no mirror, and stages its deletion", async () => {
    const stage = panel([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([]));
    expect(rows()).toEqual(["dataPrimaryweb-2", "data-oldDetachedweb-1 · Kept until you delete it"]);
    expect(screen.getByRole("note").textContent).toContain("No mirror");
    await choose("data-old options", "Delete");
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [id("web-1")] }, []);
  });

  it("offers the mirror when the Primary's Server stops answering, and stages it as Primary", () => {
    const stage = panel([member("web-1", "unanswered"), mirrorAsOf("web-2", new Date(2026, 9, 10, 10, 36))], row(["web-2"]));
    expect(screen.getByRole("alert").textContent).toBe("web-1 is not answering ·Use mirror on web-2· Changes after 10:36 lost");
    fireEvent.click(screen.getByRole("button", { name: "Use mirror on web-2" }));
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("words a staged move like Details, and locks edits while a Deploy runs it", () => {
    panel([member("web-1", "writer"), pooled("web-3"), pooled("web-4")], row([], { primary: "web-3" }, "dep-1"));
    expect(rows()[0]).toBe("dataPrimaryStagedweb-3");
    expect(screen.getByText("Move data to web-3")).toBeTruthy();
    expect(screen.getByText("Copies data to web-3 first, then a short stop.")).toBeTruthy();
    expect(screen.getByText("Deploying")).toBeTruthy();
    expect(screen.getByRole("button", { name: "data options" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "Add mirror" }).hasAttribute("disabled")).toBe(true);
  });
});
