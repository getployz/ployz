import { describe, expect, it } from "vitest";
import type { RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { runWords, trayMoving } from "./volume-copies";
import type { VolumeRunView } from "./volume-run";
import { run } from "./volume-run.test-fixture";

const volume = { id: "v1", name: "data" };

describe("trayMoving", () => {
  const NAME = "app-production_vol-v1";
  const copy = (machineId: string, role: RuntimeVolumeCopy["role"], name = NAME): RuntimeVolumeCopy => ({ machineId, name, role });
  const tray = (copies: RuntimeVolumeCopy[], active: VolumeRunView | null = null) => trayMoving({ volume, copies, active });

  it("names the Move's target from its run, not the switching source", () => {
    expect(tray([copy("web-1", "switching"), copy("web-2", "slot")], run("move", { to: "web-2" }, "running"))).toBe("Moving to web-2");
  });

  it("says a mirror refresh nowhere, as it changes nothing the tray shows", () => {
    expect(tray([copy("web-1", "writer"), copy("web-2", "slot")], run("sync", { full: false }, "running"))).toBeNull();
  });

  it("names no target for a switching copy with no Move running", () => {
    expect(tray([copy("web-1", "switching"), copy("web-2", "slot")])).toBe("Moving");
  });

  it("leaves a settled mirror to its own node, and only this Volume's copies count", () => {
    expect(tray([copy("web-1", "writer"), copy("web-2", "slot")])).toBeNull();
    expect(tray([copy("web-2", "switching", "app-production_vol-v2")])).toBeNull();
  });
});

describe("runWords", () => {
  it("says each run in placement words", () => {
    expect(runWords(run("move", { to: "web-2" }, "running"))).toBe("Moving to web-2");
    expect(runWords(run("mirror", { to: "web-3" }, "running"))).toBe("Copying to web-3");
    expect(runWords(run("sync", { full: true }, "running"))).toBe("Refreshing mirror");
    expect(runWords(run("delete_mirror", { slot: "web-2", confirmed_name: null }, "running"))).toBe("Deleting the copy on web-2");
    expect(runWords(run("restore", { from: "web-2" }, "requested"))).toBe("Using mirror on web-2");
  });
});
