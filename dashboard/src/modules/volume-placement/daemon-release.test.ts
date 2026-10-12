import { describe, expect, it } from "vitest";
import { placementBlockers, placementServers, placementUpgradeMessage, restoreOpen, runsPlacement } from "./daemon-release";

const server = (id: string, daemonVersion: string, more: { acceptsServices?: boolean; storage?: "stateless" | "ready" | "pool" | null } = {}) =>
  ({ id, name: id, daemonVersion, acceptsServices: more.acceptsServices ?? true, storage: more.storage ?? "pool" });

describe("runsPlacement", () => {
  it("needs a daemon from the release that ships the volume verbs", () => {
    expect(runsPlacement("0.2.4")).toBe(false);
    expect(runsPlacement("0.2.5-beta.1")).toBe(false);
    expect(runsPlacement("0.2.5")).toBe(true);
    expect(runsPlacement("0.3.0")).toBe(true);
  });

  it("hides the verbs from a Server whose version is unknown", () => {
    expect(runsPlacement("")).toBe(false);
  });
});

describe("placementBlockers", () => {
  it("names every Server with storage on an older daemon, counting unreported storage as storage", () => {
    expect(placementBlockers([
      server("db-old", "0.2.4"),
      server("unknown", "0.2.4", { storage: null }),
      server("ready", "", { storage: "ready" }),
      server("edge", "0.2.4", { storage: "stateless" }),
      server("new", "0.2.5"),
    ])).toEqual(["db-old", "unknown", "ready"]);
  });

  it("says which Servers to upgrade", () => {
    expect(placementUpgradeMessage(["db-old", "db-2"])).toBe("Every Server with storage must run Ployz 0.2.5 or later. Upgrade db-old, db-2 first.");
  });
});

describe("placementServers", () => {
  it("names only Servers that accept Services and run the verbs", () => {
    const servers = placementServers([
      server("new", "0.2.5"),
      server("cordoned", "0.2.5", { acceptsServices: false }),
      server("edge", "0.2.4", { storage: "stateless" }),
    ]);
    expect([...servers]).toEqual(["new"]);
  });

  it("names none while one Server with storage runs an older daemon", () => {
    expect([...placementServers([server("new", "0.2.5"), server("other", "0.3.0"), server("db-old", "0.2.4", { acceptsServices: false })])]).toEqual([]);
  });
});

describe("restoreOpen", () => {
  it("opens once the Primary's Server and every other Server with storage run the verbs", () => {
    expect(restoreOpen([server("primary", "0.2.5"), server("edge", "0.2.4", { storage: "stateless" })], "primary")).toBe(true);
    expect(restoreOpen([server("primary", "0.2.5"), server("db-old", "0.2.4")], "primary")).toBe(false);
    expect(restoreOpen([server("primary", "0.2.4", { storage: "stateless" })], "primary")).toBe(false);
    expect(restoreOpen([server("primary", "0.2.5")], null)).toBe(false);
  });
});
