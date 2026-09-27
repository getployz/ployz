import { describe, expect, it } from "vitest";
import { NOT_RESPONDING_AFTER_MS, serverStatus, sortServers } from "./server-status";

describe("serverStatus", () => {
  it("reads membership as one word", () => {
    expect(serverStatus({ membership: "up", runningBuilds: 0 }, 0)).toBe("online");
    expect(serverStatus({ membership: "up", runningBuilds: 2 }, 0)).toBe("building");
    expect(serverStatus({ membership: "down", runningBuilds: 0 }, 0)).toBe("offline");
    expect(serverStatus({ membership: "unknown", runningBuilds: 0 }, 0)).toBe("unknown");
    expect(serverStatus({ membership: "a-later-engine-state", runningBuilds: 0 }, 0)).toBe("unknown");
  });

  it("keeps a briefly suspect Server online until a minute has passed", () => {
    expect(serverStatus({ membership: "suspect", runningBuilds: 0 }, NOT_RESPONDING_AFTER_MS - 1)).toBe("online");
    expect(serverStatus({ membership: "suspect", runningBuilds: 1 }, 0)).toBe("building");
    expect(serverStatus({ membership: "suspect", runningBuilds: 1 }, NOT_RESPONDING_AFTER_MS)).toBe("not_responding");
  });
});

describe("sortServers", () => {
  it("puts Servers that need someone first, then sorts by name", () => {
    const sorted = sortServers([
      { name: "hel-10", status: "online" as const },
      { name: "hel-2", status: "online" as const },
      { name: "hel-3", status: "building" as const },
      { name: "hel-4", status: "offline" as const },
      { name: "hel-5", status: "not_responding" as const },
    ]);
    expect(sorted.map((server) => server.name)).toEqual(["hel-4", "hel-5", "hel-3", "hel-2", "hel-10"]);
  });
});
