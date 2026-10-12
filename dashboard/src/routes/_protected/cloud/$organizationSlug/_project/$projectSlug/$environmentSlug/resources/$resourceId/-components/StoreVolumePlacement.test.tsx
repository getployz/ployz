// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

function panel(
  members: Member[], placement: PlacementRow | null, running: VolumeRunView | null = null, accepting = members.map((entry) => entry.machine.id),
  upgrade: string[] = [], unknown: string[] = [],
) {
  const stage = vi.fn();
  render(<VolumePlacementPanel volume={{ id: "v1", name: "data", mounts: [{ service: "postgres" }] }} members={members} row={placement} running={running}
    accepting={accepting} upgrade={upgrade} unknown={unknown} disabled={false} stage={stage} />);
  return stage;
}
const primary = () => screen.getByRole("combobox", { name: "Primary" });
const primaryServer = () => primary().querySelector("[data-slot=select-value]")?.textContent;
/** Each chip in a group as `server:kind`, its kind the colour it wears, `live` when unstaged. */
const chips = (group: string) => within(screen.getByRole("list", { name: group })).queryAllByRole("listitem")
  .map((chip) => `${chip.firstElementChild?.textContent}:${chip.getAttribute("data-change") ?? "live"}`);
const chip = (group: string, server: string) => {
  const found = within(screen.getByRole("list", { name: group })).getAllByRole("listitem").find((one) => one.firstElementChild?.textContent === server);
  if (found === undefined) throw new Error(`no ${server} in ${group}`);
  return found;
};
const struck = (group: string, server: string) => chip(group, server).firstElementChild?.className.includes("line-through");
const actions = (group: string, server: string) => within(chip(group, server)).queryAllByRole("button");

describe("VolumePlacementPanel", () => {
  afterEach(cleanup);

  it("shows the Primary as a select naming who it runs, and each live mirror as a chip with its ×", async () => {
    const stage = panel([member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")], row(["web-2"]));
    expect(primaryServer()).toBe("web-1");
    expect(primary().hasAttribute("data-changed")).toBe(false);
    expect(screen.getByText("Runs postgres")).toBeTruthy();
    expect(chips("Mirrors")).toEqual(["web-2:live"]);
    expect(screen.queryByRole("list", { name: "Detached" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Add mirror" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Remove mirror on web-2" }));
    expect(stage).toHaveBeenLastCalledWith({ primary: id("web-1"), mirrors: [], deletes: [] }, [id("web-2")]);
    fireEvent.click(primary());
    const option = await screen.findByRole("option", { name: "web-2" });
    await waitFor(() => { fireEvent.click(option); expect(stage).toHaveBeenCalledTimes(2); });
    expect(stage).toHaveBeenLastCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("rings a moved Primary pink and shows the copy it leaves as a new detached chip, with no Was, Undo or ×", () => {
    panel([member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-3" }));
    expect(primaryServer()).toBe("web-3");
    expect(primary().getAttribute("data-changed")).toBe("true");
    expect(chips("Detached")).toEqual(["web-1:add"]);
    expect(chip("Detached", "web-1").textContent).toBe("web-1New");
    expect(actions("Detached", "web-1")).toEqual([]);
    expect(screen.queryByText(/staged|was|copies|kept/i)).toBeNull();
    expect(screen.queryByRole("button", { name: /undo|discard/i })).toBeNull();
  });

  it("drops a staged move when the select picks the live Primary again", async () => {
    const stage = panel([member("web-1", "writer"), pooled("web-3")], row([], { primary: "web-3" }));
    fireEvent.click(primary());
    const option = await screen.findByRole("option", { name: "web-1" });
    await waitFor(() => { fireEvent.click(option); expect(stage).toHaveBeenCalledTimes(1); });
    expect(stage).toHaveBeenLastCalledWith(null, []);
  });

  it("stages a mirror from + Add, then shows it green with no ×, and the detached copy it deletes first red and struck", async () => {
    const add = panel([member("web-1", "writer"), pooled("web-3")], null);
    fireEvent.click(screen.getByRole("button", { name: "Add mirror" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "web-3" }));
    expect(add).toHaveBeenCalledWith({ primary: id("web-1"), mirrors: [id("web-3")], deletes: [] }, []);
    cleanup();
    panel([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([], { primary: "web-2", mirrors: ["web-3"] }));
    expect(chips("Mirrors")).toEqual(["web-3:add"]);
    expect(chip("Mirrors", "web-3").textContent).toBe("web-3New");
    expect(actions("Mirrors", "web-3")).toEqual([]);
    expect(chips("Detached")).toEqual(["web-1:remove"]);
    expect(struck("Detached", "web-1")).toBe(true);
    expect(actions("Detached", "web-1")).toEqual([]);
  });

  it("strikes a removed mirror red and keeps it in its group until Deploy, with no ×", () => {
    panel([member("web-1", "writer"), member("web-2", "mirror")], row(["web-2"], { primary: "web-1" }));
    expect(primary().hasAttribute("data-changed")).toBe(false);
    expect(chips("Mirrors")).toEqual(["web-2:remove"]);
    expect(struck("Mirrors", "web-2")).toBe(true);
    expect(actions("Mirrors", "web-2")).toEqual([]);
  });

  it("deletes a live detached copy from its ×", () => {
    const stage = panel([member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")], row([]));
    expect(chips("Detached")).toEqual(["web-1:live"]);
    fireEvent.click(screen.getByRole("button", { name: "Delete copy on web-1" }));
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [id("web-1")] }, []);
  });

  it("offers the mirror when the Primary's Server stops answering, without staging the silent Primary", () => {
    const stage = panel([member("web-1", "unanswered"), mirrorAsOf("web-2", new Date(2026, 9, 10, 10, 36))], row(["web-2"]));
    expect(screen.getByRole("alert").textContent).toBe("web-1 is not answeringChanges after 10:36 lostUse mirror");
    expect(primary().hasAttribute("data-changed")).toBe(false);
    expect(chips("Mirrors")).toEqual(["web-2:live"]);
    fireEvent.click(screen.getByRole("button", { name: "Use mirror" }));
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("keeps Use mirror hidden while the mirror's daemon lacks the volume verbs", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", new Date(2026, 9, 10, 10, 36))], row(["web-2"]), null, [id("web-1")]);
    expect(screen.getByRole("alert").textContent).toBe("web-1 is not answeringChanges after 10:36 lost");
    expect(screen.queryByRole("button", { name: "Use mirror" })).toBeNull();
  });

  it("only shows the placement while a Server with storage can't say what it serves, and claims no Upgrade", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", new Date(2026, 9, 10, 10, 36)), member("web-3", "mirror")], row(["web-2"]), null, undefined, [], ["db-quiet"]);
    expect(screen.getByText("Ployz can’t tell whether db-quiet can move volumes")).toBeTruthy();
    expect(screen.queryByText(/Upgrade/)).toBeNull();
    expect(screen.getByRole("button", { name: "Use mirror" }).hasAttribute("disabled")).toBe(true);
    expect(screen.queryByRole("button", { name: /^Remove mirror on/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Delete copy on/ })).toBeNull();
    expect(primary().hasAttribute("data-disabled") || primary().hasAttribute("disabled")).toBe(true);
  });

  it("only shows the placement while a Server with storage runs an older daemon", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", new Date(2026, 9, 10, 10, 36)), member("web-3", "mirror")], row(["web-2"]), null, [], ["db-old"]);
    expect(screen.getByText("Upgrade db-old to a newer Ployz")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Use mirror" })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Remove mirror on/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Delete copy on/ })).toBeNull();
    expect(primary().hasAttribute("data-disabled") || primary().hasAttribute("disabled")).toBe(true);
  });

  it("shows the run under way, and drops the staged colours and edits while a Deploy runs the move", () => {
    panel([member("web-1", "writer"), pooled("web-3"), pooled("web-4")], row([], { primary: "web-3" }, "dep-1"));
    expect(screen.getByRole("status").textContent).toBe("Deploying");
    expect(primary().hasAttribute("data-changed")).toBe(false);
    expect(chips("Detached")).toEqual(["web-1:live"]);
    expect(screen.getByRole("button", { name: "Add mirror" }).hasAttribute("disabled")).toBe(true);
    cleanup();
    panel([member("web-1", "writer"), member("web-3", "mirror")], row(["web-3"]), run("move", { to: "web-3" }, "running"));
    expect(screen.getByRole("status").textContent).toBe("Moving to web-3");
    expect(screen.getByRole("button", { name: "Remove mirror on web-3" }).hasAttribute("disabled")).toBe(false);
  });
});
