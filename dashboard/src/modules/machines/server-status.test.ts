import { describe, expect, it } from "vitest";
import { serverListState, serverStatus, sortServers } from "./server-status";

describe("serverStatus", () => {
  it("reads membership as one word, and a suspect Server as online", () => {
    expect(serverStatus({ membership: "up", runningBuilds: 0 })).toBe("online");
    expect(serverStatus({ membership: "suspect", runningBuilds: 0 })).toBe("online");
    expect(serverStatus({ membership: "up", runningBuilds: 2 })).toBe("building");
    expect(serverStatus({ membership: "down", runningBuilds: 0 })).toBe("offline");
    expect(serverStatus({ membership: "unknown", runningBuilds: 0 })).toBe("unknown");
    expect(serverStatus({ membership: "a-later-engine-state", runningBuilds: 0 })).toBe("unknown");
  });
});

describe("sortServers", () => {
  it("puts Servers that need someone first, then sorts by name", () => {
    const sorted = sortServers([
      { name: "hel-10", status: "online" as const },
      { name: "hel-3", status: "building" as const },
      { name: "hel-2", status: "online" as const },
      { name: "hel-5", status: "unknown" as const },
      { name: "hel-4", status: "offline" as const },
    ]);
    expect(sorted.map((server) => server.name)).toEqual(["hel-4", "hel-5", "hel-2", "hel-3", "hel-10"]);
  });
});

describe("serverListState", () => {
  it("keeps the last observation only while there is one to show", () => {
    expect(serverListState("connecting", 0)).toBe("loading");
    expect(serverListState("observed", 0)).toBe("live");
    expect(serverListState("no_connection", 0)).toBe("live");
    expect(serverListState("unavailable", 3)).toBe("stale");
    expect(serverListState("unavailable", 0)).toBe("unreachable");
    expect(serverListState("unreachable", 0)).toBe("unreachable");
  });
});
