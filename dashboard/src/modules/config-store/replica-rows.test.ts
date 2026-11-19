import { describe, expect, it } from "vitest";
import { desiredReplicas, replicaChange, replicaRows, replicaValue } from "./replica-rows";

describe("replica rows", () => {
  it("starts a bare count in the Environment's region, and in any region with none", () => {
    expect(replicaRows(2, "fsn")).toEqual([{ region: "fsn", count: 2 }]);
    expect(replicaRows(2, null)).toEqual([{ region: null, count: 2 }]);
  });

  it("reads a map as a row per region, whatever the Environment's region", () => {
    expect(replicaRows({ syd: 1, fsn: 2 }, "nbg")).toEqual([{ region: "fsn", count: 2 }, { region: "syd", count: 1 }]);
  });

  it("keeps a Service that never named a region following the Environment while it has one row there", () => {
    expect(replicaValue([{ region: "fsn", count: 3 }], "fsn", 2, 2)).toBe(3);
    expect(replicaValue([{ region: null, count: 2 }], null, 1, 1)).toBe(2);
    expect(replicaValue([{ region: "fsn", count: 2 }, { region: "syd", count: 1 }], "fsn", 2, 2)).toEqual({ fsn: 2, syd: 1 });
    expect(replicaValue([{ region: "syd", count: 1 }], "fsn", 1, 1)).toEqual({ syd: 1 });
  });

  it("keeps the regions a Service names through a count edit, so a later default region doesn't move it", () => {
    const more = replicaRows({ fsn: 2 }, "fsn").map((row) => ({ ...row, count: 3 }));
    expect(replicaRows(replicaValue(more, "fsn", { fsn: 2 }, { fsn: 2 }), "syd")).toEqual([{ region: "fsn", count: 3 }]);
    // Named with `ployz set` and not yet deployed: the count it last ran with was bare.
    expect(replicaValue(more, "fsn", { fsn: 2 }, 2)).toEqual({ fsn: 3 });
  });

  it("keeps the last region named when another is removed", () => {
    const rows = replicaRows({ fsn: 2, syd: 1 }, "fsn").filter(({ region }) => region !== "syd");
    expect(replicaValue(rows, "fsn", { fsn: 2, syd: 1 }, { fsn: 2, syd: 1 })).toEqual({ fsn: 2 });
  });

  it("puts a Service deployed with a bare count back to one when its rows return to one row in the Environment's region", () => {
    // Deployed 2. Add region, then remove that row: nothing is left to deploy.
    const rows = replicaRows({ fsn: 2, syd: 1 }, "fsn").filter(({ region }) => region !== "syd");
    expect(replicaValue(rows, "fsn", 2, 2)).toBe(2);
    // With another count on the way, the count is all that changed.
    expect(replicaValue([{ region: "fsn", count: 3 }], "fsn", 2, 2)).toBe(3);
    // A row outside the Environment's region is no bare count, whatever was deployed.
    expect(replicaValue([{ region: "syd", count: 2 }], "fsn", 2, 2)).toEqual({ syd: 2 });
  });

  it("goes back to a bare count for any region, which no map can name", () => {
    expect(replicaValue([{ region: null, count: 2 }], null, { fsn: 2 }, { fsn: 2 })).toBe(2);
  });

  it("totals every region, which a Volume's writers count by", () => {
    expect(desiredReplicas({ fsn: 2, syd: 1 })).toBe(3);
    expect(desiredReplicas(2)).toBe(2);
  });

  it("words a change for Details with the regions it runs in on both sides", () => {
    expect(replicaChange(2, { fsn: 2, syd: 1 }, ["fsn", "fsn"])).toEqual(["fsn 2", "fsn 2 · syd 1"]);
    expect(replicaChange(1, 2, ["fsn", "syd"])).toEqual(["fsn 1", "syd 2"]);
    expect(replicaChange(1, 2, [null, null])).toEqual(["1", "2"]);
    expect(replicaChange(null, { fsn: 2 }, [null, null])).toEqual(["", "fsn 2"]);
  });

  it("says a count now names its region when both sides would read the same", () => {
    expect(replicaChange(2, { fsn: 2 }, ["fsn", "fsn"])).toEqual(["2", "fsn 2"]);
  });

  it("counts every region of a map as what the Service asks for, and nothing for a value it can't read", () => {
    expect(desiredReplicas({ fsn: 2, syd: 1 })).toBe(3);
    expect(desiredReplicas(2)).toBe(2);
    expect(desiredReplicas("two")).toBeNull();
    expect(desiredReplicas(undefined)).toBeNull();
  });
});
