// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { Snapshot } from "#/modules/volume-copies/volume-copies";
import { copiesFromPlacement } from "#/modules/volume-copies/volume-copies.placement";
import { offsiteCopyOf } from "#/modules/volume-copies/volume-offsite";
import type { VolumeOffsiteData } from "#/modules/volume-copies/volume-offsite.hooks";
import { snapshotClock } from "#/modules/volume-copies/volume-snapshots";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { COPIES, member, run } from "#/modules/volume-run/volume-run.test-fixture";
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
  unreached: false,
  deploymentId: null,
});
const mirrorAsOf = (server: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: "s", guid: server, created_unix_seconds: when.getTime() / 1000 } } } };
};
const server = (name: string, storage: CopyServer["storage"] = "pool"): CopyServer => ({ id: id(name), name, storage });

function panel(members: Member[], placement: PlacementRow | null, options: {
  open?: string; upgrade?: string[]; servers?: CopyServer[]; lastDone?: VolumeRunView; refreshing?: boolean; deploying?: boolean; snapshots?: Snapshot[];
  offsite?: Partial<VolumeOffsiteData>;
} = {}) {
  const stage = vi.fn();
  const refresh = vi.fn();
  const deleteCopy = vi.fn(() => Promise.resolve());
  const restore = vi.fn();
  const onOpen = vi.fn();
  const snapshots = { list: options.snapshots ? { epoch: 7, snapshots: options.snapshots } : null, pending: false, take: vi.fn(), keep: vi.fn(), remove: vi.fn() };
  const offsite: VolumeOffsiteData = {
    read: null, add: vi.fn(() => Promise.resolve(true)), upload: vi.fn(() => Promise.resolve(true)), field: null, clearField: vi.fn(),
    pending: false, uploading: false, ...options.offsite,
  };
  const stored = offsite.read?.served ? offsite.read.copy : null;
  const copies = copiesFromPlacement({
    volume, members, row: placement, active: null, lastSync: null, lastDone: options.lastDone ?? null, snapshots: options.snapshots ?? null,
    offsite: stored === null ? null : offsiteCopyOf(stored, null),
  });
  render(<VolumeCopiesPanel volume={volume} members={members} rows={placement ? [placement] : []} running={null} copies={copies}
    servers={options.servers ?? members.map((entry) => server(entry.machine.name))} accepting={members.map((entry) => entry.machine.id)}
    upgrade={options.upgrade ?? []} unknown={[]} seeds disabled={false} deploying={options.deploying ?? false} stage={stage} restore={restore}
    deleteCopy={deleteCopy} refresh={refresh} refreshing={options.refreshing ?? false}
    snapshots={snapshots} schedule={{ policy: null, pending: false, save: vi.fn(() => Promise.resolve(true)) }} snapshotClock={snapshotClock("UTC")} offsite={offsite} open={options.open ? id(options.open) : null} onOpen={onOpen} now={NOW} />);
  return { stage, deleteCopy, restore, onOpen, refresh, snapshots, offsite };
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

  const silent = () => [member("web-1", "writer"), member("web-2", "unanswered")];
  const doneAgo = (minutes: number) => ({ ...run("sync", { full: false }, "done"), finished_at: new Date(NOW.getTime() - minutes * 60_000).toISOString() });
  const refreshNow = () => screen.getByRole("button", { name: "Refresh now" }) as HTMLButtonElement;

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
    panel(silent(), row(["web-2"]), { open: "web-2", lastDone: doneAgo(135) });
    expect(screen.getByText("Refresh skipped")).toBeTruthy();
    expect(screen.getByText(/^web-2 is not answering\. Ployz retries every 15 minutes\. Until it catches up, a failover loses 2 hours of writes\.$/)).toBeTruthy();
    expect(refreshNow().disabled).toBe(false);
  });

  it("hides the skipped prose while a round runs", () => {
    panel(silent(), row(["web-2"]), { open: "web-2", lastDone: doneAgo(135), refreshing: true });
    expect(screen.queryByText("Refresh skipped")).toBeNull();
  });

  it("falls back to the list when the copy is gone", () => {
    panel(fresh(), row(["web-2"]), { open: "web-9" });
    expect(screen.getByRole("list", { name: "Live" })).toBeTruthy();
  });
});

const snap = (id: string, iso: string, extra: Partial<Snapshot> = {}): Snapshot =>
  ({ id, at: new Date(iso), kind: "manual", note: null, kept: false, bytes: 20e6, losesBytes: 0, ...extra });
const BUCKET = {
  id: "5f1c2b8e-0000-4000-8000-000000000001", endpoint: "https://s3.eu-central-1.amazonaws.com", region: "eu-central-1", bucket: "acme-offsite",
  prefix: "ployz", accessKey: "AKIAEXAMPLE", secretSet: true,
} as const;
const OFFSITE_READ = {
  served: true, buckets: [BUCKET], source: "g1",
  copy: { bucket: BUCKET, keep: 7, uploadedAt: new Date("2026-10-12T09:05:00Z"), backups: [{ id: "off-1", at: new Date("2026-10-12T09:00:00Z"), bytes: 2e9, note: "daily" }] },
} as const;
const SNAPS = [snap("s2", "2026-10-12T10:00:00Z", { losesBytes: 1.2e9 }), snap("s1", "2026-10-11T09:30:00Z", { kind: "daily", kept: true })];

describe("Snapshots", () => {
  afterEach(cleanup);

  it("stays out of the Live group where the writer can't list snapshots", () => {
    panel([member("web-1", "writer")], row([]));
    expect(rows("Live")).toEqual(["web-1PrimaryLive"]);
  });

  it("opens from the Live group and lists snapshots by day", () => {
    panel([member("web-1", "writer")], row([]), { snapshots: SNAPS });
    fireEvent.click(screen.getByRole("button", { name: /^Snapshots/ }));
    expect(rows("Today")).toEqual(["10:00Manual20 MBRestore"]);
    expect(rows("Yesterday")).toEqual(["09:30Daily20 MBRestore"]);
    expect(screen.getByLabelText("Kept")).toBeTruthy();
  });

  it("names what a restore loses and stages it on the writer", async () => {
    const { stage } = panel([member("web-1", "writer")], row([]), { snapshots: SNAPS });
    fireEvent.click(screen.getByRole("button", { name: /^Snapshots/ }));
    fireEvent.click(group("Today").getByRole("button", { name: "Restore" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("Loses").closest("[data-slot=field]")?.textContent).toBe("Loses1.2 GB · 2 hours of writes");
    expect(within(dialog).getByText("Manual · 10:00 swaps in")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
    expect(stage).toHaveBeenCalledWith(
      expect.objectContaining({ primary: id("web-1"), restore: expect.objectContaining({ from: "s2", at: 1_791_799_200, losesBytes: 1.2e9, expect: 7 }) }),
      [],
    );
  });

  it("says nothing is lost when nothing was written since", async () => {
    panel([member("web-1", "writer")], row([]), { snapshots: SNAPS });
    fireEvent.click(screen.getByRole("button", { name: /^Snapshots/ }));
    fireEvent.click(group("Yesterday").getByRole("button", { name: "Restore" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("Loses").closest("[data-slot=field]")?.textContent).toBe("LosesNothing");
  });

  it("marks the staged snapshot pink and shows the restore on the Live group", () => {
    const staged: PlacementRow = { ...row([]), staged: {
      primary: id("web-1"), mirrors: [], deletes: [], restore: { from: "s2", at: 1_791_799_200, losesBytes: 0, save: "x", expect: 7 },
    } };
    panel([member("web-1", "writer")], staged, { snapshots: SNAPS });
    expect(rows("Live")[1]).toMatch(/^Restore.* · on deploy$/);
    fireEvent.click(screen.getByRole("button", { name: /^Snapshots/ }));
    const item = group("Today").getByRole("listitem");
    expect(item.getAttribute("data-staged")).toBe("restore");
    expect(within(item).getByText("Staged")).toBeTruthy();
    expect(within(item).queryByRole("button", { name: "Restore" })).toBeNull();
  });

  it("offers no restore while a move is staged", () => {
    panel([member("web-1", "writer"), pooled("web-2")], row([], { primary: "web-2" }), { snapshots: SNAPS });
    fireEvent.click(screen.getByRole("button", { name: /^Snapshots/ }));
    expect(screen.queryByRole("button", { name: "Restore" })).toBeNull();
  });

  it("takes a snapshot now", () => {
    const { snapshots } = panel([member("web-1", "writer")], row([]), { snapshots: SNAPS });
    fireEvent.click(screen.getByRole("button", { name: /^Snapshots/ }));
    fireEvent.click(screen.getByRole("button", { name: "Take snapshot" }));
    expect(snapshots.take).toHaveBeenCalledOnce();
  });
  it("adds an offsite copy into a New bucket and shows the writer's refusal in the field", async () => {
    const { offsite } = panel([member("web-1", "writer")], row([]), {
      offsite: { read: { served: true, buckets: [], copy: null, source: "g1" }, field: "Access denied for AKIAEXAMPLE" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add copy" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("This runs now, not on deploy.")).toBeTruthy();
    expect(within(dialog).getByRole("radio", { name: /^New bucket/ }).getAttribute("aria-checked")).toBe("true");
    expect(within(dialog).getByText("Access denied for AKIAEXAMPLE")).toBeTruthy();
    expect(within(dialog).getByText("Daily · 7 kept")).toBeTruthy();
    const add = within(dialog).getByRole("button", { name: "Add copy" });
    expect(add.hasAttribute("disabled")).toBe(true);
    const type = (label: string | RegExp, value: string) => fireEvent.change(within(dialog).getByLabelText(label, { selector: "input" }), { target: { value } });
    type("Endpoint", "s3.eu-central-1.amazonaws.com");
    type("Bucket", "acme-offsite");
    type("Access key", "AKIAEXAMPLE");
    type(/^Secret key/, "hunter2");
    expect(offsite.clearField).toHaveBeenCalled();
    fireEvent.click(add);
    expect(offsite.add).toHaveBeenCalledWith({
      kind: "new", endpoint: "s3.eu-central-1.amazonaws.com", bucket: "acme-offsite", accessKey: "AKIAEXAMPLE", secretKey: "hunter2",
    });
  });

  it("offers a stored bucket by name and region", async () => {
    const { offsite } = panel([member("web-1", "writer")], row([]), { offsite: { read: { served: true, buckets: [BUCKET], copy: null, source: "g1" } } });
    fireEvent.click(screen.getByRole("button", { name: "Add copy" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("radio", { name: /^acme-offsite/ }).textContent).toBe("acme-offsiteeu-central-1");
    fireEvent.click(within(dialog).getByRole("button", { name: "Add copy" }));
    expect(offsite.add).toHaveBeenCalledWith({ kind: "existing", id: BUCKET.id });
  });

  it("restores a backup through the restore dialog with its download size", async () => {
    const { stage } = panel([member("web-1", "writer")], row([]), { snapshots: SNAPS, offsite: { read: OFFSITE_READ } });
    expect(rows("Elsewhere")).toEqual([expect.stringMatching(/^acme-offsiteOffsite · Daily · 7 kept/)]);
    fireEvent.click(screen.getByRole("button", { name: "Offsite on acme-offsite" }));
    expect(screen.getByText("acme-offsite · eu-central-1")).toBeTruthy();
    expect(rows("Backups")).toEqual(["09:00daily2.0 GBRestore"]);
    fireEvent.click(group("Backups").getByRole("button", { name: "Restore" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("Downloads 2.0 GB")).toBeTruthy();
    expect(within(dialog).getByText("Offsite · 09:00 swaps in")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
    expect(stage).toHaveBeenCalledWith(expect.objectContaining({
      primary: id("web-1"),
      restore: expect.objectContaining({ from: "off-1", expect: 7, offsite: { bucket: BUCKET.id, bytes: 2e9 } }),
    }), []);
  });

  it("keeps offsite out of Add copy where the writer doesn't serve it", () => {
    panel([member("web-1", "writer")], row([]), { offsite: { read: { served: false } } });
    expect(screen.queryByRole("button", { name: "Add copy" })).toBeNull();
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
