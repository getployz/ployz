// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { copiesFromPlacement } from "#/modules/volume-copies/volume-copies.placement";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import { type CopyServer, SafetyLine, VolumeCopiesPanel } from "./StoreVolumeCopies";

const NOW = new Date("2026-10-12T12:00:00Z");
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const volume = { id: "v1", name: "data", mounts: [{ service: "postgres" }] };
const pooled = (server: string) => member(server, "empty", { pool: true });
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: (staged.deletes ?? []).map(id) },
  deploymentId: null,
});
const mirrorAsOf = (server: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: "s", guid: server, created_unix_seconds: when.getTime() / 1000 } } } };
};
const server = (name: string, storage: CopyServer["storage"] = "pool"): CopyServer => ({ id: id(name), name, storage });

function panel(members: Member[], placement: PlacementRow | null, options: { open?: string; upgrade?: string[]; servers?: CopyServer[]; deploying?: boolean } = {}) {
  const stage = vi.fn();
  const deleteCopy = vi.fn(() => Promise.resolve());
  const restore = vi.fn();
  const onOpen = vi.fn();
  const copies = copiesFromPlacement({ volume, members, row: placement, active: null, lastSync: null, lastDone: null });
  render(<VolumeCopiesPanel volume={volume} members={members} rows={placement ? [placement] : []} running={null} copies={copies}
    servers={options.servers ?? members.map((entry) => server(entry.machine.name))} accepting={members.map((entry) => entry.machine.id)}
    upgrade={options.upgrade ?? []} seeds disabled={false} deploying={options.deploying ?? false} stage={stage} restore={restore} deleteCopy={deleteCopy}
    open={options.open ? id(options.open) : null} onOpen={onOpen} now={NOW} />);
  return { stage, deleteCopy, restore, onOpen };
}
const group = (label: string) => within(screen.getByRole("list", { name: label }));
const rows = (label: string) => group(label).queryAllByRole("listitem").map((item) => item.textContent);

describe("VolumeCopiesPanel", () => {
  afterEach(cleanup);

  it("says what dies with the server when nothing is elsewhere, and adds a mirror through the dialog", async () => {
    const { stage } = panel([member("web-1", "writer"), pooled("web-2"), pooled("web-3")], row([]),
      { servers: [server("web-1"), server("edge", "stateless"), server("web-3")] });
    expect(rows("Live")).toEqual(["web-1PrimaryLive"]);
    expect(screen.getByText("Nothing outside web-1. If web-1 dies, data goes with it.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add copy" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("Add copy · data")).toBeTruthy();
    const radio = (name: string) => within(dialog).getByRole("radio", { name: new RegExp(`^${name}`) });
    expect(radio("web-1").textContent).toBe("web-1Live");
    expect(radio("web-1").hasAttribute("disabled")).toBe(true);
    expect(radio("edge").textContent).toBe("edgeDocker only");
    expect(radio("edge").hasAttribute("disabled")).toBe(true);
    const add = within(dialog).getByRole("button", { name: "Add copy" });
    expect(add.hasAttribute("disabled")).toBe(true);
    fireEvent.click(radio("web-3"));
    expect(radio("web-3").getAttribute("aria-checked")).toBe("true");
    fireEvent.click(add);
    expect(stage).toHaveBeenCalledWith({ primary: id("web-1"), mirrors: [id("web-3")], deletes: [] }, []);
  });

  it("lists the mirror and the old copy elsewhere, and hides Add copy once a mirror exists", () => {
    const { onOpen } = panel([member("web-1", "writer"), mirrorAsOf("web-2", new Date(NOW.getTime() - 5 * 60_000)), member("web-3", "mirror")], row(["web-2"]));
    expect(rows("Elsewhere")).toEqual([expect.stringMatching(/^web-2Mirror · 15 min · 5 min/), expect.stringMatching(/^web-3Old copy · /)]);
    expect(screen.queryByRole("button", { name: "Add copy" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Mirror on web-2" }));
    expect(onOpen).toHaveBeenCalledWith(id("web-2"));
  });

  it("offers Delete on an old copy once the Deploy that stamped the Volume has ended, and not while it runs", () => {
    const stamped = { ...row([]), deploymentId: "dep-1" };
    panel([member("web-1", "writer"), member("web-3", "mirror")], stamped);
    expect(screen.getByRole("button", { name: "Delete copy on web-3" }).hasAttribute("disabled")).toBe(false);
    cleanup();
    panel([member("web-1", "writer"), member("web-3", "mirror")], stamped, { deploying: true });
    expect(screen.getByRole("button", { name: "Delete copy on web-3" }).hasAttribute("disabled")).toBe(true);
  });

  it("deletes an old copy at once after a plain confirm, not staged", async () => {
    const { stage, deleteCopy } = panel([member("web-1", "writer"), member("web-3", "mirror")], row([]));
    fireEvent.click(screen.getByRole("button", { name: "Delete copy on web-3" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("Delete copy")).toBeTruthy();
    expect(within(dialog).getByText(/^web-3 · data · /)).toBeTruthy();
    expect(within(dialog).getByText("This runs now, not on deploy. It can't be undone.")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(deleteCopy).toHaveBeenCalledWith("web-3"));
    expect(stage).not.toHaveBeenCalled();
  });

  it("closes the delete confirm at once, without waiting for Cloud", async () => {
    const { deleteCopy } = panel([member("web-1", "writer"), member("web-3", "mirror")], row([]));
    deleteCopy.mockReturnValue(new Promise(() => undefined));
    fireEvent.click(screen.getByRole("button", { name: "Delete copy on web-3" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(deleteCopy).toHaveBeenCalledWith("web-3");
  });

  it("restores from an old copy's latest point, as from a mirror's", () => {
    const { restore } = panel([member("web-1", "writer"), mirrorAsOf("web-3", new Date(NOW.getTime() - 3_600_000))], row([]));
    fireEvent.click(screen.getByRole("button", { name: "Restore from web-3" }));
    expect(restore).toHaveBeenCalledWith(expect.objectContaining({ guid: "web-3" }));
  });

  it("shows a staged mirror green with New and no action, Was or Undo", () => {
    panel([member("web-1", "writer"), pooled("web-2")], row([], { primary: "web-1", mirrors: ["web-2"] }));
    const item = group("Elsewhere").getByRole("listitem");
    expect(item.textContent).toBe("web-2Mirror · on deployNew");
    expect(item.getAttribute("data-staged")).toBe("new");
    expect(screen.queryByText(/was|undo|staged/i)).toBeNull();
    expect(screen.queryByRole("button", { name: /delete|discard/i })).toBeNull();
  });

  it("only shows while a Server needs a newer Ployz", () => {
    panel([member("web-1", "writer"), member("web-3", "mirror"), pooled("web-2")], row([]), { upgrade: ["web-2"] });
    expect(screen.getByText("Upgrade web-2 to a newer Ployz")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Add copy" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete copy on web-3" })).toBeNull();
  });
});

describe("mirror detail", () => {
  afterEach(cleanup);
  const fresh = () => [member("web-1", "writer"), mirrorAsOf("web-2", new Date(NOW.getTime() - 5 * 60_000))];

  it("stages Make primary and Remove mirror, and goes back to the list", () => {
    const { stage, onOpen } = panel(fresh(), row(["web-2"]), { open: "web-2" });
    expect(screen.getByText("Every 15 min · on deploy")).toBeTruthy();
    expect(screen.getByText("Make primary moves postgres to web-2 on deploy. Writes pause for the final sync.")).toBeTruthy();
    expect(screen.getByText("Deletes the copy on web-2 when you deploy.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    expect(stage).toHaveBeenLastCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
    fireEvent.click(screen.getByRole("button", { name: "Remove mirror" }));
    expect(stage).toHaveBeenLastCalledWith({ primary: id("web-1"), mirrors: [], deletes: [] }, [id("web-2")]);
    fireEvent.click(screen.getByRole("button", { name: "Copies" }));
    expect(onOpen).toHaveBeenCalledWith(null);
  });

  it("warns that a behind mirror catches up first", () => {
    panel([member("web-1", "writer"), mirrorAsOf("web-2", new Date(NOW.getTime() - 3 * 3_600_000))], row(["web-2"]), { open: "web-2" });
    expect(screen.getByText(/The deploy catches up first, then pauses writes for the final sync\. If web-2 can't catch up, the deploy fails and web-1 keeps serving\./)).toBeTruthy();
  });

  it("restores from the mirror's latest point", () => {
    const { restore } = panel(fresh(), row(["web-2"]), { open: "web-2" });
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    expect(restore).toHaveBeenCalledWith(expect.objectContaining({ guid: "web-2" }));
  });

  it("falls back to the list when the copy is gone", () => {
    panel(fresh(), row(["web-2"]), { open: "web-9" });
    expect(screen.getByRole("list", { name: "Live" })).toBeTruthy();
  });
});

describe("SafetyLine", () => {
  afterEach(cleanup);

  it("warns with no copies and counts them otherwise", () => {
    const lone = copiesFromPlacement({ volume, members: [member("web-1", "writer")], row: row([]), active: null, lastSync: null, lastDone: null });
    const mirrored = copiesFromPlacement({ volume, members: [member("web-1", "writer"), mirrorAsOf("web-2", NOW)], row: row(["web-2"]), active: null, lastSync: null, lastDone: null });
    if (lone === null || mirrored === null) throw new Error("fixture");
    render(<SafetyLine copies={lone} now={NOW} />);
    expect(screen.getByText("No copies").parentElement?.getAttribute("data-tone")).toBe("warn");
    cleanup();
    render(<SafetyLine copies={mirrored} now={NOW} />);
    expect(screen.getByText("1 copy").parentElement?.getAttribute("data-tone")).toBe("ok");
  });
});
