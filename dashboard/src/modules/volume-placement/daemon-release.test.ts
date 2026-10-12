import { describe, expect, it } from "vitest";
import { requirements } from "#/modules/runtime/daemon-compat";
import {
  placementBlockers, placementLacking, placementServers, placementUnknown, placementUnknownMessage, placementUpgradeMessage, restoreOpen, runsPlacement,
} from "./daemon-release";

type Daemon = "serves" | "lacks" | "0.2.3" | "0.2.4" | "unknown";

const daemonOf = (daemon: Daemon) =>
  daemon === "serves" ? { daemonVersion: "0.2.5", capabilities: ["ployz.logs.tail.v1", requirements.volumeMove.capability] }
  : daemon === "lacks" ? { daemonVersion: "0.2.5", capabilities: ["ployz.logs.tail.v1"] }
  : daemon === "unknown" ? { daemonVersion: "dev", capabilities: [] }
  : { daemonVersion: daemon, capabilities: [] };

const server = (id: string, daemon: Daemon, more: { acceptsServices?: boolean; storage?: "stateless" | "ready" | "pool" | null } = {}) =>
  ({ id, name: id, ...daemonOf(daemon), acceptsServices: more.acceptsServices ?? true, storage: more.storage ?? "pool" });

describe("runsPlacement", () => {
  it("needs a daemon that serves the volume move capability", () => {
    expect(runsPlacement(daemonOf("lacks"))).toBe(false);
    expect(runsPlacement(daemonOf("serves"))).toBe(true);
  });

  it("reads a daemon too old to list capabilities by its version", () => {
    expect(runsPlacement(daemonOf("0.2.3"))).toBe(false);
    expect(runsPlacement(daemonOf("0.2.4"))).toBe(false);
    expect(runsPlacement({ daemonVersion: "0.2.5", capabilities: [] })).toBe(true);
    expect(runsPlacement(daemonOf("unknown"))).toBe(false);
  });
});

describe("placementBlockers", () => {
  it("names every Server with storage that lacks the verbs, counting unreported storage as storage", () => {
    expect(placementBlockers([
      server("db-old", "lacks"),
      server("unknown", "lacks", { storage: null }),
      server("ready", "unknown", { storage: "ready" }),
      server("edge", "lacks", { storage: "stateless" }),
      server("new", "serves"),
    ])).toEqual(["db-old", "unknown", "ready"]);
  });

  it("claims a lack only for the blockers whose records say so, and leaves the silent ones unknown", () => {
    const servers = [server("db-old", "lacks"), server("ready", "unknown", { storage: "ready" }), server("edge", "unknown", { storage: "stateless" }), server("new", "serves")];
    expect(placementLacking(servers)).toEqual(["db-old"]);
    expect(placementUnknown(servers)).toEqual(["ready"]);
    expect(placementUnknownMessage(["ready"])).toBe("Ployz can't tell whether ready can move volumes.");
  });

  it("claims a lack, so an Upgrade, for released 0.2.3 and 0.2.4 daemons that list no capabilities", () => {
    const servers = [server("db-3", "0.2.3"), server("db-4", "0.2.4", { storage: null }), server("new", "serves")];
    expect(placementLacking(servers)).toEqual(["db-3", "db-4"]);
    expect(placementUnknown(servers)).toEqual([]);
  });

  it("says which Servers to upgrade", () => {
    expect(placementUpgradeMessage(["db-old", "db-2"])).toBe("Every Server with storage needs a newer Ployz. Upgrade db-old, db-2 first.");
  });
});

describe("placementServers", () => {
  it("names only Servers that accept Services and run the verbs", () => {
    const servers = placementServers([
      server("new", "serves"),
      server("cordoned", "serves", { acceptsServices: false }),
      server("edge", "lacks", { storage: "stateless" }),
    ]);
    expect([...servers]).toEqual(["new"]);
  });

  it("names none while one Server with storage lacks the verbs", () => {
    expect([...placementServers([server("new", "serves"), server("other", "serves"), server("db-old", "lacks", { acceptsServices: false })])]).toEqual([]);
  });
});

describe("restoreOpen", () => {
  it("opens once the Primary's Server and every other Server with storage run the verbs", () => {
    expect(restoreOpen([server("primary", "serves"), server("edge", "lacks", { storage: "stateless" })], "primary")).toBe(true);
    expect(restoreOpen([server("primary", "serves"), server("db-old", "lacks")], "primary")).toBe(false);
    expect(restoreOpen([server("primary", "lacks", { storage: "stateless" })], "primary")).toBe(false);
    expect(restoreOpen([server("primary", "serves")], null)).toBe(false);
  });
});
