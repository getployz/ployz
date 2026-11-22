import { describe, expect, it } from "vitest";
import { movedRouting, requirements } from "#/modules/runtime/daemon-compat";
import { moveServers, placementBlockers, placementServers, placementUpgradeMessage, restoreOpen, runsPlacement, unroutedServers } from "./daemon-release";

type Daemon = "serves" | "lacks" | "silent";

const daemonOf = (daemon: Daemon) =>
  daemon === "serves" ? { capabilities: ["ployz.logs.tail.v1", requirements.volumeMove.capability] }
  : daemon === "lacks" ? { capabilities: ["ployz.logs.tail.v1"] }
  : { capabilities: [] };

const server = (id: string, daemon: Daemon, more: { acceptsServices?: boolean; storage?: "stateless" | "ready" | "pool" | null } = {}) =>
  ({ id, name: id, ...daemonOf(daemon), acceptsServices: more.acceptsServices ?? true, storage: more.storage === undefined ? "pool" : more.storage });

describe("runsPlacement", () => {
  it("needs a daemon that lists the volume move capability", () => {
    expect(runsPlacement(daemonOf("serves"))).toBe(true);
    expect(runsPlacement(daemonOf("lacks"))).toBe(false);
  });

  it("reads a record that lists no capabilities as lacking the verbs", () => {
    expect(runsPlacement(daemonOf("silent"))).toBe(false);
  });

  it("no longer accepts a daemon that serves only the old mirror verb", () => {
    expect(runsPlacement({ capabilities: ["ployz.volume.mirror.declare.v1"] })).toBe(false);
  });
});

describe("placementBlockers", () => {
  it("names every Server with storage that lacks the verbs, counting unreported storage and silent records", () => {
    expect(placementBlockers([
      server("db-old", "lacks"),
      server("unknown", "lacks", { storage: null }),
      server("ready", "silent", { storage: "ready" }),
      server("edge", "silent", { storage: "stateless" }),
      server("new", "serves"),
    ])).toEqual(["db-old", "unknown", "ready"]);
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

describe("moveServers", () => {
  // As 0.3.0-beta.6 lists them: it takes a moved Volume, then never routes the copy it starts.
  const beta6 = { capabilities: [requirements.volumeMove.capability, requirements.volumeFloor.capability] };
  const current = { capabilities: [...beta6.capabilities, movedRouting.capability] };
  const at = (id: string, daemon: { capabilities: string[] }) => ({ ...server(id, "serves"), ...daemon });

  it("leaves out a Server whose Ployz can't route a moved copy, so a Drain moves onto a current one", () => {
    const machines = [at("web-2", beta6), at("web-3", current)];
    expect([...moveServers(machines)]).toEqual(["web-3"]);
    expect([...unroutedServers(machines)]).toEqual(["web-2"]);
  });
});
