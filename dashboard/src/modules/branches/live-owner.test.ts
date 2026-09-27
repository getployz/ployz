import { describe, expect, it } from "vitest";
import { liveOwner } from "./live-owner";

// production ⑂ staging ⑂ fix-web
const branches = [
  { environmentId: "staging", parentEnvironmentId: "production" },
  { environmentId: "fix-web", parentEnvironmentId: "staging" },
];
const applied = new Map([
  ["production", new Set(["postgres", "redis"])],
  ["staging", new Set(["redis"])],
]);

describe("liveOwner", () => {
  it("is the Parent when the Parent runs the node", () => {
    expect(liveOwner("staging", "redis", branches, applied)).toBe("staging");
  });

  it("is the nearest ancestor that runs it when the Parent doesn't", () => {
    expect(liveOwner("staging", "postgres", branches, applied)).toBe("production");
  });

  it("is nobody when no ancestor runs it", () => {
    expect(liveOwner("staging", "search", branches, applied)).toBeNull();
  });
});
