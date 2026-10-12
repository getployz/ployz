import { describe, expect, it } from "vitest";
import { requirements } from "#/modules/runtime/daemon-compat";
import {
  placementBlockers, placementLacking, placementServers, placementUnknown, placementUnknownMessage, placementUpgradeMessage, restoreOpen, runsPlacement,
} from "./daemon-release";

type Daemon = "serves" | "lacks" | "unknown";

const capabilitiesOf = (daemon: Daemon) =>
  daemon === "serves" ? ["ployz.logs.tail.v1", requirements.volumeMove.capability] : daemon === "lacks" ? ["ployz.logs.tail.v1"] : [];

const server = (id: string, daemon: Daemon, more: { acceptsServices?: boolean; storage?: "stateless" | "ready" | "pool" | null } = {}) =>
  ({ id, name: id, capabilities: capabilitiesOf(daemon), acceptsServices: more.acceptsServices ?? true, storage: more.storage ?? "pool" });

describe("runsPlacement", () => {
  it("needs a daemon that serves the volume move capability", () => {
    expect(runsPlacement({ capabilities: capabilitiesOf("lacks") })).toBe(false);
    expect(runsPlacement({ capabilities: capabilitiesOf("serves") })).toBe(true);
  });

  it("hides the verbs from a Server that lists no capabilities", () => {
    expect(runsPlacement({ capabilities: [] })).toBe(false);
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
