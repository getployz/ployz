import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import { member, run } from "#/modules/volume-run/volume-run.test-fixture";
import { copiesFromPlacement } from "./volume-copies.placement";
import { cardLine, safetyLine, traySpeech } from "./volume-copies";

const volume = { id: "v1", name: "pg-data" };
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: (staged.deletes ?? []).map(id) },
  deploymentId: null,
});
type Run = ReturnType<typeof run> | null;
const copies = (placement: PlacementRow | null, members = [member("web-1", "writer"), member("web-2", "mirror")], active: Run = null, lastSync: Run = null, lastDone: Run = null) => {
  const model = copiesFromPlacement({ volume, members, row: placement, active, lastSync, lastDone });
  if (model === null) throw new Error("no primary");
  return model;
};
const summary = (model: ReturnType<typeof copies>) => model.elsewhere.map((copy) => `${copy.kind} ${"server" in copy ? copy.server.name : ""} ${copy.staged ?? "-"} ${copy.busy ?? "-"}`);

describe("copiesFromPlacement", () => {
  it("puts the primary in Live and the mirror Elsewhere, named data-<server>", () => {
    const model = copies(row(["web-2"]));
    expect(model.live.server?.name).toBe("web-1");
    expect(model.elsewhere[0]).toMatchObject({ kind: "mirror", name: "pg-data-web-2", skipped: false });
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

  it("marks the mirror skipped when its last refresh failed", () => {
    expect(copies(row(["web-2"]), undefined, null, run("sync", { full: false }, "failed")).elsewhere[0]).toMatchObject({ skipped: true });
  });

  it("ages a mirror whose Server is silent from the last round that ended done, and marks it skipped", () => {
    const now = new Date("2026-10-09T03:02:00.000Z");
    const done = { ...run("sync", { full: false }, "done"), finished_at: "2026-10-09T00:00:00.000Z" };
    const failed = run("sync", { full: false }, "failed");
    const model = copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "unanswered")], null, failed, done);
    const [mirror] = model.elsewhere;
    expect(mirror).toMatchObject({ kind: "mirror", skipped: true, refreshedAt: new Date("2026-10-09T00:00:00.000Z") });
    expect(safetyLine(model, now)).toEqual({ tone: "warn", words: "Mirror behind", detail: "3 hours ago" });
    expect(mirror && cardLine(mirror, now).stale).toBe(true);
  });

  it("marks a silent mirror skipped with no age when no round ever ended done", () => {
    const model = copies(row(["web-2"]), [member("web-1", "writer"), member("web-2", "unanswered")]);
    expect(model.elsewhere[0]).toMatchObject({ kind: "mirror", skipped: true, refreshedAt: null });
  });

  it("has no snapshots until the Servers serve them", () => {
    expect(copies(row(["web-2"])).live.snapshots).toBeNull();
  });
});
