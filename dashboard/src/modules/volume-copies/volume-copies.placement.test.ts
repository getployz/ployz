import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import { COPIES, member, run } from "#/modules/volume-run/volume-run.test-fixture";
import { copiesFromPlacement, type MirrorStatusView } from "./volume-copies.placement";
import { cardLine, traySpeech } from "./volume-copies";

const volume = { id: "v1", name: "pg-data" };
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: (staged.deletes ?? []).map(id) },
  unreached: false,
  deploymentId: null,
});
type Run = ReturnType<typeof run> | null;
const status = (server: string, seen: Partial<Omit<MirrorStatusView, "machine">>): MirrorStatusView =>
  ({ machine: id(server), okAt: null, matchedAt: null, failedAt: null, failure: null, ...seen });
const copies = (placement: PlacementRow | null, members = [member("web-1", "writer"), member("web-2", "mirror")], active: Run = null, statuses: MirrorStatusView[] = []) => {
  const model = copiesFromPlacement({ volume, members, row: placement, active, statuses });
  if (model === null) throw new Error("no primary");
  return model;
};
const summary = (model: ReturnType<typeof copies>) => model.elsewhere.map((copy) => `${copy.kind} ${"server" in copy ? copy.server.name : ""} ${copy.staged ?? "-"} ${copy.busy ?? "-"}`);

describe("copiesFromPlacement", () => {
  it("puts the primary in Live and the mirror Elsewhere, named data-<server>", () => {
    const model = copies(row(["web-2"]));
    expect(model.live.server?.name).toBe("web-1");
    expect(model.elsewhere[0]).toMatchObject({ kind: "mirror", name: "pg-data-web-2", skipped: null });
    expect(traySpeech(model)).toBeNull();
  });

  it("badges a staged new mirror and a staged removal", () => {
    expect(summary(copies(row([], { primary: "web-1", mirrors: ["web-2"] }), [member("web-1", "writer"), member("web-2", "empty")])))
      .toEqual(["mirror web-2 new -"]);
    expect(summary(copies(row(["web-2"], { primary: "web-1" })))).toEqual(["mirror web-2 removing -"]);
  });

  it("badges the mirror staged to become primary and moves the tray to it", () => {
    const model = copies(row(["web-2"], { primary: "web-2" }));
    expect(model.live.server?.name).toBe("web-1");
    expect(summary(model)).toEqual(["mirror web-2 primary -", "old web-1 new -"]);
    expect(traySpeech(model)).toEqual({ tone: "staged", words: "Moving to web-2" });
  });

  it("lists a copy the dashboard does not mirror as an old copy, red once staged for deletion", () => {
    expect(summary(copies(row([])))).toEqual(["old web-2 - -"]);
    expect(summary(copies(row([], { primary: "web-1", deletes: ["web-2"] })))).toEqual(["old web-2 removing -"]);
  });

  it("puts what runs on the copy it runs on, and a move on the tray", () => {
    expect(summary(copies(row(["web-2"]), undefined, run("sync", { full: false }, "running")))).toEqual(["mirror web-2 - Refreshing"]);
    expect(summary(copies(row(["web-2"]), undefined, run("delete_mirror", { slot: "web-2", confirmed_name: null }, "running"))))
      .toEqual(["mirror web-2 - Removing"]);
    const moving = copies(row(["web-2"]), undefined, run("move", { to: "web-3" }, "running"));
    expect(traySpeech(moving)).toEqual({ tone: "busy", words: "Moving to web-3" });
  });

  it("marks the mirror skipped by a failed pull when its Server answers", () => {
    const failed = status("web-2", { failedAt: "2026-10-09T01:00:00.000Z", failure: "no space" });
    expect(copies(row(["web-2"]), undefined, null, [failed]).elsewhere[0]).toMatchObject({ skipped: "failed" });
    expect(copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "failed")]).elsewhere[0]).toMatchObject({ skipped: "failed" });
  });

  it("keeps an older failure over a pull that landed before it, and clears it once a pull lands after", () => {
    const failedAt = "2026-10-09T01:00:00.000Z";
    const before = status("web-2", { okAt: "2026-10-09T00:00:00.000Z", failedAt, failure: "no space" });
    const after = status("web-2", { okAt: "2026-10-09T02:00:00.000Z", failedAt, failure: "no space" });
    expect(copies(row(["web-2"]), undefined, null, [before]).elsewhere[0]).toMatchObject({ skipped: "failed" });
    expect(copies(row(["web-2"]), undefined, null, [after]).elsewhere[0]).toMatchObject({ skipped: null });
  });

  it("marks the mirror Refreshing while its pull runs", () => {
    expect(summary(copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "pulling")]))).toEqual(["mirror web-2 - Refreshing"]);
  });

  it("ages a mirror whose Server is silent from the last sync that found it current, and marks it skipped", () => {
    const now = new Date("2026-10-09T03:02:00.000Z");
    const seen = status("web-2", { okAt: "2026-10-09T00:00:00.000Z", matchedAt: "2026-10-09T00:00:00.000Z" });
    const model = copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "unanswered")], null, [seen]);
    const [mirror] = model.elsewhere;
    expect(mirror).toMatchObject({ kind: "mirror", skipped: "silent", refreshedAt: new Date("2026-10-09T00:00:00.000Z") });
    expect(mirror && cardLine(mirror, now).stale).toBe(true);
  });

  it("ages an answering mirror from the last sync that found it current", () => {
    const snapshotAt = new Date(1_790_000_000 * 1000);
    const now = new Date(snapshotAt.getTime() + 2 * 3_600_000);
    const matchedAt = new Date(now.getTime() - 5 * 60_000).toISOString();
    const model = copies(row(["web-2"]), undefined, null, [status("web-2", { okAt: matchedAt, matchedAt })]);
    const [mirror] = model.elsewhere;
    expect(mirror).toMatchObject({ kind: "mirror", skipped: null, refreshedAt: new Date(now.getTime() - 5 * 60_000) });
    expect(mirror && cardLine(mirror, now)).toMatchObject({ stale: false, age: "5 minutes ago" });
  });

  it("marks a silent mirror skipped with no age when no sync ever found it current", () => {
    const model = copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "unanswered")]);
    expect(model.elsewhere[0]).toMatchObject({ kind: "mirror", skipped: "silent", refreshedAt: null });
  });

  it("marks only the mirror Refreshing while a refresh runs, leaving an old copy free", () => {
    const model = copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "mirror")], run("sync", { full: false }, "running"));
    expect(summary(model)).toEqual(["mirror web-2 - Refreshing", "old web-3 - -"]);
  });

  it("reads a recorded mirror that never refreshed as behind, not as protection", () => {
    const now = new Date("2026-10-09T03:02:00.000Z");
    const fresh = { ...member("web-2", "mirror"), view: { copy: { ...COPIES.mirror, newest: null } } };
    const matchedAt = "2026-10-09T03:00:00.000Z";
    const model = copies(row(["web-2"]), [member("web-1", "writer"), fresh], null, [status("web-2", { okAt: matchedAt, matchedAt })]);
    const [mirror] = model.elsewhere;
    expect(mirror).toMatchObject({ kind: "mirror", refreshedAt: null });
    expect(mirror && cardLine(mirror, now).stale).toBe(true);
  });

  it("has no snapshots until the Servers serve them", () => {
    expect(copies(row(["web-2"])).live.snapshots).toBeNull();
  });
});
