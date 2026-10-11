// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PlacementRow, RestorePoint } from "#/modules/volume-placement/volume-placement";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import { VolumeRestorePoints } from "./StoreVolumeBackups";

const id = (server: string) => server.padEnd(32, "0") as MachineId;
const volume = { id: "v1", name: "data", deployed: true, storage: { kind: "provisioned", maximumBytes: 1e9 } } as const;
const at = (iso: string) => new Date(iso);
/** A mirror on `server` whose latest point is `guid`, made at `when`. */
const mirrorAt = (server: string, guid: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: `w-${guid}`, guid, created_unix_seconds: when.getTime() / 1000 } } } };
};
const seedRow = (guid: string, when: Date): PlacementRow => ({
  volumeId: "v2", environmentId: "e1", mirrors: [], deploymentId: null,
  staged: {
    primary: id("a"), mirrors: [], deletes: [],
    seed: { sourceVolumeId: "v1", sourceName: "data", dockerVolume: "data-1027", point: { guid, name: `w-${guid}`, at: when.getTime() / 1000 } },
  },
});
const ownRow = (deploymentId: string | null): PlacementRow => ({ volumeId: "v1", environmentId: "e1", mirrors: [id("b")], staged: null, deploymentId });

function points(members: Member[] | undefined, rows: PlacementRow[] = []) {
  const restore = vi.fn<(point: RestorePoint) => void>();
  const onAddMirror = vi.fn();
  render(<VolumeRestorePoints volume={volume} members={members} rows={rows} disabled={false} restore={restore} onAddMirror={onAddMirror} />);
  return { restore, onAddMirror };
}
const rows = () => within(screen.getByRole("list", { name: "Restore points" })).getAllByRole("listitem");
const firstRow = () => {
  const [row] = rows();
  if (row === undefined) throw new Error("no rows");
  return within(row);
};

afterEach(cleanup);

describe("VolumeRestorePoints", () => {
  it("lists each mirror's latest point newest first, with its server", () => {
    points([member("a", "writer"), mirrorAt("b", "20", at("2026-10-10T10:27:00Z")), mirrorAt("c", "21", at("2026-10-10T11:02:00Z"))]);
    expect(rows().map((row) => row.textContent)).toEqual([
      expect.stringContaining("c"),
      expect.stringContaining("b"),
    ]);
    expect(screen.getByText("Each mirror keeps its latest point. Refreshed on every deploy and every 15 minutes.")).toBeTruthy();
  });

  it("shows one row when two mirrors hold the same point", () => {
    const when = at("2026-10-10T10:27:00Z");
    points([member("a", "writer"), mirrorAt("b", "20", when), mirrorAt("c", "20", when)]);
    expect(rows()).toHaveLength(1);
  });

  it("restores the clicked point", () => {
    const when = at("2026-10-10T10:27:00Z");
    const { restore } = points([member("a", "writer"), mirrorAt("b", "20", when)]);
    fireEvent.click(firstRow().getByRole("button", { name: "Restore" }));
    expect(restore).toHaveBeenCalledWith(expect.objectContaining({ guid: "20", at: when, server: expect.objectContaining({ name: "b" }) }));
  });

  it("marks a point whose restore is staged, without a second Restore", () => {
    const when = at("2026-10-10T10:27:00Z");
    points([member("a", "writer"), mirrorAt("b", "20", when)], [seedRow("20", when)]);
    expect(firstRow().getByText("Staged")).toBeTruthy();
    expect(firstRow().queryByRole("button", { name: "Restore" })).toBeNull();
  });

  it("locks the other points while a restore is staged", () => {
    const when = at("2026-10-10T10:27:00Z");
    points([member("a", "writer"), mirrorAt("b", "20", when), mirrorAt("c", "21", at("2026-10-10T11:02:00Z"))], [seedRow("20", when)]);
    expect(firstRow().getByRole("button", { name: "Restore" })).toHaveProperty("disabled", true);
  });

  it("locks Restore while the Volume's own placement deploys", () => {
    points([member("a", "writer"), mirrorAt("b", "20", at("2026-10-10T10:27:00Z"))], [ownRow("d1")]);
    expect(firstRow().getByRole("button", { name: "Restore" })).toHaveProperty("disabled", true);
  });

  it("says a mirror makes points when there is none, and offers to add one", () => {
    const { onAddMirror } = points([member("a", "writer")]);
    expect(screen.getByText("No restore points")).toBeTruthy();
    expect(screen.queryByRole("list", { name: "Restore points" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Add mirror" }));
    expect(onAddMirror).toHaveBeenCalledOnce();
  });

  it("waits for the servers before saying anything", () => {
    points(undefined);
    expect(screen.getByText("Waiting for your servers.")).toBeTruthy();
  });
});
