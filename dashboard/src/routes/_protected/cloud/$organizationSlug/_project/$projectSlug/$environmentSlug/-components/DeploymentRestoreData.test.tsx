// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { deploySnapshotId } from "#/modules/volume-copies/snapshot-schedule";
import type { Snapshot } from "#/modules/volume-copies/volume-copies";
import { snapshotClock } from "#/modules/volume-copies/volume-snapshots";
import type { PlacementRow } from "#/modules/volume-placement/volume-placement";
import type { Member } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { RestoreDataRow } from "./DeploymentRestoreData";

const id = (server: string) => server.padEnd(32, "0") as MachineId;
const BEFORE = deploySnapshotId("before", "dep-1");
const snapshot = (overrides: Partial<Snapshot> = {}): Snapshot => ({
  id: BEFORE, at: new Date("2026-10-12T10:00:00Z"), kind: "before_deploy", kept: false, note: "#12", bytes: 1e6, losesBytes: 3e8, ...overrides,
});
const row = (staged: PlacementRow["staged"] = null): PlacementRow => ({ volumeId: "v1", environmentId: "e1", mirrors: [], staged, unreached: false, deploymentId: null });

function show(options: { snapshots?: Snapshot[] | null; members?: Member[]; row?: PlacementRow } = {}) {
  const stage = vi.fn();
  render(<ul><RestoreDataRow deploymentId="dep-1" data={{
    volume: { id: "v1", name: "db", mounts: [{ service: "api" }] },
    members: options.members ?? [member("web-1", "writer")],
    rows: [options.row ?? row()],
    upgrade: [],
    unknown: [],
    disabled: false,
    deploying: false,
    stage,
    snapshotClock: snapshotClock("UTC"),
    snapshots: { list: options.snapshots === null ? null : { epoch: 7, snapshots: options.snapshots ?? [snapshot()] } },
    live: "web-1",
  }} /></ul>);
  return stage;
}
const items = () => screen.queryAllByRole("listitem").map((item) => item.textContent);

describe("RestoreDataRow", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date("2026-10-12T12:00:00Z"));
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("offers this deploy's Before snapshot and stages its restore on the writer", async () => {
    const stage = show();
    expect(items()).toEqual(["dbBefore deploy · 10:00300 MB written sinceRestore"]);
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
    expect(stage).toHaveBeenCalledWith(
      expect.objectContaining({ primary: id("web-1"), restore: expect.objectContaining({ from: BEFORE, losesBytes: 3e8, expect: 7 }) }),
      [],
    );
  });

  it("says Unchanged when nothing was written since", () => {
    show({ snapshots: [snapshot({ losesBytes: 0 })] });
    expect(items()).toEqual(["dbBefore deploy · 10:00UnchangedRestore"]);
  });

  it("leaves out a Volume without this deploy's snapshot, or whose writer can't list snapshots", () => {
    show({ snapshots: [snapshot({ id: deploySnapshotId("before", "dep-0") })] });
    show({ snapshots: null });
    expect(items()).toEqual([]);
  });

  it("shows a staged restore as Staged, with no Restore button", () => {
    show({ row: row({ primary: id("web-1"), mirrors: [], deletes: [], restore: { from: BEFORE, at: 0, losesBytes: 0, save: "x", expect: 7 } }) });
    expect(items()).toEqual(["dbBefore deploy · 10:00300 MB written sinceStaged"]);
  });

  it("offers no restore while a move is staged", () => {
    show({ members: [member("web-1", "writer"), member("web-2", "empty", { pool: true })], row: row({ primary: id("web-2"), mirrors: [], deletes: [] }) });
    expect(items()).toEqual([]);
  });
});
