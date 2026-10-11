import { describe, expect, it } from "vitest";
import { placementServers, runsPlacement } from "./daemon-release";

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

describe("placementServers", () => {
  it("names only Servers that accept Services and run the verbs", () => {
    const servers = placementServers([
      { id: "old", acceptsServices: true, daemonVersion: "0.2.4" },
      { id: "new", acceptsServices: true, daemonVersion: "0.2.5" },
      { id: "cordoned", acceptsServices: false, daemonVersion: "0.2.5" },
    ]);
    expect([...servers]).toEqual(["new"]);
  });
});
