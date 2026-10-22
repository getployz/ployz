// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { RoleGate } from "#/modules/organization/members.queries";
import { copiesFromPlacement } from "#/modules/volume-copies/volume-copies.placement";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import { type CopyServer, VolumeCopiesPanel } from "./StoreVolumeCopies";

const NOW = new Date("2026-10-12T12:00:00Z");
const DAY = 86_400_000;
const clock = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit" });
const dated = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const volume = { id: "v1", name: "data", mounts: [{ service: "postgres" }] };
const pooled = (server: string) => member(server, "empty");
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: (staged.deletes ?? []).map(id) },
  unreached: false,
  deploymentId: null,
});
const copyAsOf = (copy: "mirror" | "old") => (server: string, when: Date): Member => {
  const base = member(server, copy);
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES[copy], newest: { name: "s", guid: server, created_unix_seconds: when.getTime() / 1000 } } } };
};
const mirrorAsOf = copyAsOf("mirror");
const oldAsOf = copyAsOf("old");
const server = (name: string, storage: CopyServer["storage"] = "pool"): CopyServer => ({ id: id(name), name, storage });

function panel(members: Member[], placement: PlacementRow | null, options: {
  open?: string; upgrade?: string[]; servers?: CopyServer[]; matchedAt?: string; refreshing?: boolean; deploying?: boolean; owed?: boolean;
  forgetGate?: RoleGate; floorless?: string[]; seeds?: boolean; rows?: PlacementRow[];
} = {}) {
  const stage = vi.fn();
  const refresh = vi.fn();
  const deleteCopy = vi.fn(() => Promise.resolve());
  const restore = vi.fn();
  const onOpen = vi.fn();
  const copies = copiesFromPlacement({ volume, members, row: placement, active: null,
    statuses: options.matchedAt === undefined ? [] : [{ machine: id("web-2"), okAt: options.matchedAt, matchedAt: options.matchedAt, failedAt: null, failure: null }] });
  const router = createRouter({ routeTree: createRootRoute(), history: createMemoryHistory() });
  render(<RouterContextProvider router={router}><VolumeCopiesPanel organizationSlug="ada" forgetGate={options.forgetGate ?? { allowed: true, reason: undefined }} volume={volume} members={members} rows={options.rows ?? (placement ? [placement] : [])} running={null} owed={options.owed ?? false} copies={copies}
    servers={options.servers ?? members.map((entry) => server(entry.machine.name))} accepting={members.map((entry) => entry.machine.id)} floorless={(options.floorless ?? []).map(id)}
    upgrade={options.upgrade ?? []} seeds={options.seeds ?? true} disabled={false} deploying={options.deploying ?? false} stage={stage} restore={restore}
    deleteCopy={deleteCopy} refresh={refresh} refreshing={options.refreshing ?? false}
    open={options.open ? id(options.open) : null} onOpen={onOpen} /></RouterContextProvider>);
  return { stage, deleteCopy, restore, onOpen, refresh };
}
const group = (label: string) => within(screen.getByRole("list", { name: label }));
const rows = (label: string) => group(label).queryAllByRole("listitem").map((item) => item.textContent);

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
});
afterEach(() => vi.useRealTimers());

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
    const { onOpen } = panel([member("web-1", "writer"), mirrorAsOf("web-2", new Date(NOW.getTime() - 5 * 60_000)), member("web-3", "old")], row(["web-2"]));
    expect(rows("Elsewhere")).toEqual([expect.stringMatching(/^web-2Mirror · 15 min · 5 min/), expect.stringMatching(/^web-3Old copy · /)]);
    expect(screen.queryByRole("button", { name: "Add copy" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Mirror on web-2" }));
    expect(onOpen).toHaveBeenCalledWith(id("web-2"));
  });

  it("offers Delete on an old copy once the Deploy that stamped the Volume has ended, and not while it runs", () => {
    const stamped = { ...row([]), deploymentId: "dep-1" };
    panel([member("web-1", "writer"), member("web-3", "old")], stamped);
    expect(screen.getByRole("button", { name: "Delete copy on web-3" }).hasAttribute("disabled")).toBe(false);
    cleanup();
    panel([member("web-1", "writer"), member("web-3", "old")], stamped, { deploying: true });
    expect(screen.getByRole("button", { name: "Delete copy on web-3" }).hasAttribute("disabled")).toBe(true);
  });

  it("deletes an old copy at once after a plain confirm, not staged", async () => {
    const { stage, deleteCopy } = panel([member("web-1", "writer"), member("web-3", "old")], row([]));
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
    const { deleteCopy } = panel([member("web-1", "writer"), member("web-3", "old")], row([]));
    deleteCopy.mockReturnValue(new Promise(() => undefined));
    fireEvent.click(screen.getByRole("button", { name: "Delete copy on web-3" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(deleteCopy).toHaveBeenCalledWith("web-3");
  });

  it("restores from an old copy's latest point, as from a mirror's", () => {
    const { restore } = panel([member("web-1", "writer"), oldAsOf("web-3", new Date(NOW.getTime() - 3_600_000))], row([]));
    fireEvent.click(screen.getByRole("button", { name: "Restore from web-3" }));
    expect(restore).toHaveBeenCalledWith(expect.objectContaining({ guid: "web-3" }));
  });

  it("keeps an old copy's staged restore Staged while Restore isn't offered, and offers no new one", () => {
    const old = oldAsOf("web-3", new Date(NOW.getTime() - 3_600_000));
    const point = { guid: "web-3", name: "web-3-point", at: 0 };
    const restoring: PlacementRow = { ...row([]), volumeId: "restored",
      staged: { primary: id("web-1"), mirrors: [], deletes: [], seed: { sourceVolumeId: volume.id, sourceName: volume.name, dockerVolume: "ns_vol-restored", point } } };
    panel([member("web-1", "writer"), old], row([]), { seeds: false, rows: [row([]), restoring] });
    expect(group("Elsewhere").getByText("Staged")).toBeTruthy();
    cleanup();
    panel([member("web-1", "writer"), old], row([]), { seeds: false });
    expect(screen.queryByRole("button", { name: "Restore from web-3" })).toBeNull();
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
    panel([member("web-1", "writer"), member("web-3", "old"), pooled("web-2")], row([]), { upgrade: ["web-2"] });
    expect(screen.getByText("Upgrade web-2 to a newer Ployz")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Add copy" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete copy on web-3" })).toBeNull();
  });

  const fiveAgo = () => new Date(NOW.getTime() - 5 * 60_000);
  const useMirror = () => screen.queryByRole("button", { name: "Use mirror" });

  it("sends an admin to forget the silent Primary's Server, with no Use mirror while it is silent", () => {
    const { stage } = panel([member("web-1", "unanswered"), mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(screen.getByText("web-1 is not answering")).toBeTruthy();
    expect(screen.getByText(`Changes after ${clock.format(fiveAgo())} lost. The mirror on web-2 can take over once web-1 is forgotten.`, { exact: false })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Forget web-1" }).getAttribute("href")).toMatch(new RegExp(`/~/servers/${id("web-1")}$`));
    expect(useMirror()).toBeNull();
    expect(stage).not.toHaveBeenCalled();
  });

  it("names the silent Primary's Server under Live as not answering, never Live, with no time", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(rows("Live")).toEqual(["web-1PrimaryNot answering"]);
  });

  it("tells a member an owner or admin has to forget the silent Server, with no link", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", fiveAgo())], row(["web-2"]),
      { forgetGate: { allowed: false, reason: "Only an owner or admin can forget servers." } });
    expect(screen.getByText(/can take over once web-1 is forgotten\. Only an owner or admin can forget servers\.$/)).toBeTruthy();
    expect(screen.queryByRole("link")).toBeNull();
    expect(useMirror()).toBeNull();
  });

  it("names and links every silent Server, since each one holds Use mirror back", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", fiveAgo()), member("web-3", "unanswered")], row(["web-2"]));
    expect(screen.getByText("web-1 and web-3 are not answering")).toBeTruthy();
    expect(screen.getByText(/once web-1 and web-3 are forgotten\./)).toBeTruthy();
    expect(screen.getAllByRole("link").map((link) => link.textContent)).toEqual(["Forget web-1", "Forget web-3"]);
    expect(useMirror()).toBeNull();
  });

  it("lists the surviving mirror once the silent Server is forgotten, and stages it as primary", () => {
    const { stage } = panel([mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(screen.getByText("data has no primary")).toBeTruthy();
    expect(rows("Elsewhere")).toEqual([`web-2Mirror · ${clock.format(fiveAgo())}`]);
    expect(screen.queryByText("No server reports a copy.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Use mirror" }));
    expect(stage).toHaveBeenCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
  });

  it("names the Server that can't report its copy, not a silent one, while that Server may hold the writer", () => {
    const lacking: Member = { machine: member("web-3", "unanswered").machine, answered: false, lack: "Volume move is not in Ployz 0.3.0 on web-3." };
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", fiveAgo()), lacking], row(["web-2"]));
    expect(screen.getByText("web-3 can't report this copy")).toBeTruthy();
    expect(rows("Live")).toEqual(["web-3PrimaryCan't report this copy"]);
    expect(screen.queryByRole("link")).toBeNull();
    expect(useMirror()).toBeNull();
  });

  it("keeps the silent Server's alert and its Forget link in the mirror's detail, where an outage lands", () => {
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", fiveAgo())], row(["web-2"]), { open: "web-2" });
    expect(screen.getByRole("button", { name: "Copies" })).toBeTruthy();
    expect(screen.getByText("web-1 is not answering")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Forget web-1" }).getAttribute("href")).toMatch(new RegExp(`/~/servers/${id("web-1")}$`));
    expect(useMirror()).toBeNull();
  });

  it("names the Server to upgrade when that is what holds Use mirror back after the Forget", () => {
    panel([mirrorAsOf("web-2", fiveAgo())], row(["web-2"]), { upgrade: ["web-3"] });
    expect(screen.getByText("Upgrade web-3 to a newer Ployz")).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Use mirror" }).disabled).toBe(true);
  });

  it("waits when the Primary's report hasn't arrived, with no Forget for another silent Server, as its Live row says", () => {
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([unlisted, mirrorAsOf("web-2", fiveAgo()), member("web-3", "unanswered")], row(["web-2"]));
    expect(screen.getByText("Waiting for every server's report")).toBeTruthy();
    expect(screen.queryByText(/take over|Forget/)).toBeNull();
    expect(rows("Live")).toEqual([`${id("web-1")}PrimaryWaiting for its report`]);
  });

  it("names the Primary whose report is pending, not a silent Server listed before it, and waits with no Forget", () => {
    // observeVolumeMembers lists the frame's Servers, silent ones among them, and appends those it names only by id.
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([mirrorAsOf("web-2", fiveAgo()), member("web-3", "unanswered"), unlisted], row(["web-2"]));
    expect(rows("Live")).toEqual([`${id("web-1")}PrimaryWaiting for its report`]);
    expect(screen.getByText("Waiting for every server's report")).toBeTruthy();
    expect(screen.queryByText(/take over|Forget|not answering/)).toBeNull();
  });

  it("says what the presumed primary's Server can't do instead of claiming the Volume has no primary", () => {
    const lacking: Member = { machine: member("web-1", "unanswered").machine, answered: false, lack: "Volume move is not in Ployz 0.3.0 on web-1." };
    panel([lacking, mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(screen.getByText("web-1 can't report this copy")).toBeTruthy();
    expect(screen.getByText(/Volume move is not in Ployz 0\.3\.0 on web-1\.$/)).toBeTruthy();
    expect(screen.queryByText("data has no primary")).toBeNull();
    expect(screen.getByRole("alert").className).not.toContain("text-destructive");
    expect(useMirror()).toBeNull();
    cleanup();
    panel([{ ...lacking, lack: "Volume move needs a newer Ployz on web-1. 0.3.1 is out.", outdated: true }, mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(screen.getByText("web-1 needs a newer Ployz")).toBeTruthy();
  });

  it("names the Primary whose report is pending beside a silent staged target, with no move the row didn't stage", () => {
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([mirrorAsOf("web-2", fiveAgo()), member("web-3", "unanswered"), unlisted], row(["web-2"], { primary: "web-3", mirrors: ["web-2"] }));
    expect(rows("Live")).toEqual([`${id("web-1")}PrimaryWaiting for its report`]);
    expect(screen.queryByText(/No primary|Moving to/)).toBeNull();
  });

  it("names a Primary whose root is read-only Read-only under Live, not Live", () => {
    panel([member("web-1", "cut"), mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(rows("Live")).toEqual(["web-1PrimaryRead-only"]);
  });

  it("waits with the staged primary's report pending, as Live names it, though another Server is silent and listed first", () => {
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([member("web-3", "unanswered"), mirrorAsOf("web-2", fiveAgo()), unlisted], row(["web-2"], { primary: "web-1" }));
    expect(screen.getByText("Waiting for every server's report")).toBeTruthy();
    expect(screen.queryByText(/take over|Forget/)).toBeNull();
    expect(rows("Live")).toEqual([`${id("web-1")}PrimaryWaiting for its report`]);
  });

  it("keeps a silent mirror staged as primary under Elsewhere while the writer's report is on its way, staged, with nothing to stage again", () => {
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([member("web-2", "unanswered"), unlisted], row(["web-2"], { primary: "web-2" }), { open: "web-2" });
    expect(screen.getByText("web-2 is not answering.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Make primary" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove mirror" })).toBeNull();
  });

  it("offers no edit on a silent mirror that staging holds as the primary while Live names the Server whose report is on its way", () => {
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([member("web-2", "unanswered"), unlisted], row(["web-2"]), { open: "web-2" });
    expect(screen.getByText("web-2 is not answering.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Make primary" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove mirror" })).toBeNull();
  });

  it("waits for a Server's report when the frame lists it only by id, with no Forget link and no Use mirror", () => {
    const unlisted: Member = { machine: { id: id("web-1"), name: id("web-1") }, answered: false, incomplete: true };
    panel([unlisted, mirrorAsOf("web-2", fiveAgo())], row(["web-2"]));
    expect(screen.getByText("Waiting for every server's report")).toBeTruthy();
    expect(screen.getByText("Copies shows what you can do once every server has reported.")).toBeTruthy();
    expect(screen.getByRole("alert").className).not.toContain("text-destructive");
    expect(screen.queryByText(/not answering|take over/)).toBeNull();
    expect(rows("Live")).toEqual([`${id("web-1")}PrimaryWaiting for its report`]);
    expect(screen.queryByRole("link")).toBeNull();
    expect(useMirror()).toBeNull();
  });

  it("says the mirror's Server needs a newer Ployz instead of offering a restore the Deploy refuses", () => {
    panel([mirrorAsOf("web-2", fiveAgo())], row(["web-2"]), { floorless: ["web-2"] });
    expect(screen.getByText(/Using a mirror needs a newer Ployz on web-2\.$/)).toBeTruthy();
    expect(useMirror()).toBeNull();
  });

  it("says why it can't use the mirror while a committed Move is owed", () => {
    panel([mirrorAsOf("web-2", fiveAgo())], row(["web-2"]), { owed: true });
    expect(screen.getByText(/data's last move hasn't finished\.$/)).toBeTruthy();
    expect(useMirror()).toBeNull();
  });

  it("shows the staged restore without an alert, the mirror as staged primary", () => {
    panel([mirrorAsOf("web-2", fiveAgo())], row(["web-2"], { primary: "web-2" }));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(rows("Live")).toEqual(["No primaryMoving to web-2 · on deploy"]);
    expect(group("Elsewhere").getByRole("listitem").getAttribute("data-staged")).toBe("primary");
  });

  it("says no server reports a copy of a Volume never deployed", () => {
    panel([pooled("web-1"), pooled("web-2")], null);
    expect(screen.getByText("No server reports a copy.")).toBeTruthy();
  });

  it("dates an old copy's and a silent Primary's restore point from another day", async () => {
    const at = new Date(NOW.getTime() - 3 * DAY);
    panel([member("web-1", "writer"), oldAsOf("web-3", at)], row([]));
    expect(rows("Elsewhere")).toEqual([expect.stringContaining(`Old copy · ${dated.format(at)}`)]);
    fireEvent.click(screen.getByRole("button", { name: "Delete copy on web-3" }));
    expect(within(await screen.findByRole("alertdialog")).getByText(`web-3 · data · ${dated.format(at)}`)).toBeTruthy();
    cleanup();
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", at)], row(["web-2"]));
    expect(screen.getByText(`Changes after ${dated.format(at)} lost`, { exact: false })).toBeTruthy();
  });

  it("dates an old copy's restore point once the day turns, with the Servers' answer unchanged", () => {
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    vi.setSystemTime(new Date(2026, 9, 11, 23, 59));
    const at = new Date(2026, 9, 11, 23, 55);
    panel([member("web-1", "writer"), oldAsOf("web-3", at)], row([]));
    expect(rows("Elsewhere")).toEqual([expect.stringContaining(`Old copy · ${clock.format(at)}`)]);
    act(() => vi.advanceTimersByTime(2 * 60_000));
    expect(rows("Elsewhere")).toEqual([expect.stringContaining(`Old copy · ${dated.format(at)}`)]);
  });

  it("gives an old copy's and a silent Primary's restore point from today the time alone", async () => {
    const at = new Date(NOW.getTime() - 5 * 60_000);
    panel([member("web-1", "writer"), oldAsOf("web-3", at)], row([]));
    expect(rows("Elsewhere")).toEqual([expect.stringContaining(`Old copy · ${clock.format(at)}`)]);
    fireEvent.click(screen.getByRole("button", { name: "Delete copy on web-3" }));
    expect(within(await screen.findByRole("alertdialog")).getByText(`web-3 · data · ${clock.format(at)}`)).toBeTruthy();
    cleanup();
    panel([member("web-1", "unanswered"), mirrorAsOf("web-2", at)], row(["web-2"]));
    expect(screen.getByText(`Changes after ${clock.format(at)} lost`, { exact: false })).toBeTruthy();
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

  it("warns that a mirror with no refresh yet catches up first", () => {
    const base = member("web-2", "mirror");
    const unrefreshed: Member = base.answered ? { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: null } } } : base;
    panel([member("web-1", "writer"), unrefreshed], row(["web-2"]), { open: "web-2" });
    expect(screen.getByText(/The deploy catches up first, then pauses writes for the final sync\./)).toBeTruthy();
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

  it("gives a restore point from today the time alone", () => {
    panel(fresh(), row(["web-2"]), { open: "web-2" });
    expect(screen.getByText(clock.format(new Date(NOW.getTime() - 5 * 60_000)))).toBeTruthy();
  });

  it("dates a restore point from another day, so an older point reads apart from today's", () => {
    const at = new Date(NOW.getTime() - 3 * DAY);
    panel([member("web-1", "writer"), mirrorAsOf("web-2", at)], row(["web-2"]), { open: "web-2" });
    expect(screen.getByText(dated.format(at))).toBeTruthy();
  });

  it("dates the restore point once the day turns, with the Servers' answer unchanged", () => {
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    vi.setSystemTime(new Date(2026, 9, 11, 23, 59));
    const at = new Date(2026, 9, 11, 23, 55);
    panel([member("web-1", "writer"), mirrorAsOf("web-2", at)], row(["web-2"]), { open: "web-2" });
    expect(screen.getByText(clock.format(at))).toBeTruthy();
    act(() => vi.advanceTimersByTime(2 * 60_000));
    expect(screen.getByText(dated.format(at))).toBeTruthy();
  });

  const silent = () => [member("web-1", "writer"), member("web-2", "unanswered")];
  const matchedAgo = (minutes: number) => new Date(NOW.getTime() - minutes * 60_000).toISOString();
  const refreshNow = () => screen.getByRole("button", { name: "Refresh now" }) as HTMLButtonElement;

  it("says a mirror's Server isn't answering on its row and in its detail minutes after it stopped, beside its age", () => {
    panel(silent(), row(["web-2"]), { matchedAt: matchedAgo(9) });
    expect(rows("Elsewhere")).toEqual(["web-2Mirror · 15 min · 9 minutes ago · Not answering"]);
    cleanup();
    panel(silent(), row(["web-2"]), { open: "web-2", matchedAt: matchedAgo(9) });
    expect(screen.getByText("web-2 is not answering.")).toBeTruthy();
    expect(screen.getByText("9 minutes ago")).toBeTruthy();
    expect(screen.queryByText("Refresh skipped")).toBeNull();
  });

  it("says a never-refreshed mirror's Server isn't answering without claiming a refresh was skipped", () => {
    panel(silent(), row(["web-2"]), { open: "web-2" });
    expect(screen.getByText("web-2 is not answering.")).toBeTruthy();
    expect(screen.queryByText("Refresh skipped")).toBeNull();
  });

  it("runs a round on Refresh now", () => {
    const { refresh } = panel(fresh(), row(["web-2"]), { open: "web-2" });
    expect(screen.getByText("5 minutes ago")).toBeTruthy();
    fireEvent.click(refreshNow());
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("reads Refreshing from the click until the round ends, and can't be clicked again", () => {
    panel(fresh(), row(["web-2"]), { open: "web-2", refreshing: true });
    expect(screen.getByText("Refreshing")).toBeTruthy();
    expect(refreshNow().disabled).toBe(true);
  });

  it("says why a silent mirror's refresh was skipped", () => {
    panel(silent(), row(["web-2"]), { open: "web-2", matchedAt: matchedAgo(135) });
    expect(screen.getByText("Refresh skipped")).toBeTruthy();
    expect(screen.getByText(/^web-2 is not answering\. Ployz retries every 15 minutes\. Until it catches up, a failover loses 2 hours of writes\.$/)).toBeTruthy();
    expect(refreshNow().disabled).toBe(false);
  });

  it("says a mirror whose report hasn't arrived waits for it, not that its Server isn't answering", () => {
    const unlisted: Member = { machine: { id: id("web-2"), name: id("web-2") }, answered: false, incomplete: true };
    panel([member("web-1", "writer"), unlisted], row(["web-2"]), { open: "web-2", matchedAt: matchedAgo(135) });
    expect(screen.getByText(/^Waiting for its server's report\. Ployz retries every 15 minutes\. Until it catches up, a failover loses 2 hours of writes\.$/)).toBeTruthy();
    expect(screen.queryByText(/not answering/)).toBeNull();
  });

  it("says a fresh mirror waits for its server's report without claiming a refresh was skipped", () => {
    const unlisted: Member = { machine: { id: id("web-2"), name: id("web-2") }, answered: false, incomplete: true };
    panel([member("web-1", "writer"), unlisted], row(["web-2"]), { open: "web-2", matchedAt: matchedAgo(20) });
    expect(screen.getByText("Waiting for its server's report.")).toBeTruthy();
    expect(screen.queryByText("Refresh skipped")).toBeNull();
  });

  it("hides the skipped prose while a round runs", () => {
    panel(silent(), row(["web-2"]), { open: "web-2", matchedAt: matchedAgo(135), refreshing: true });
    expect(screen.queryByText("Refresh skipped")).toBeNull();
  });

  it("falls back to the list when the copy is gone", () => {
    panel(fresh(), row(["web-2"]), { open: "web-9" });
    expect(screen.getByRole("list", { name: "Live" })).toBeTruthy();
  });
});
