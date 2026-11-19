import { describe, expect, it } from "vitest";
import { replicaRows, replicaText, replicaTotal, replicaValue } from "./replica-rows";

describe("replica rows", () => {
  it("starts a bare count in the Environment's region, and in any region with none", () => {
    expect(replicaRows(2, "fsn")).toEqual([{ region: "fsn", count: 2 }]);
    expect(replicaRows(2, null)).toEqual([{ region: null, count: 2 }]);
  });

  it("reads a map as a row per region, whatever the Environment's region", () => {
    expect(replicaRows({ syd: 1, fsn: 2 }, "nbg")).toEqual([{ region: "fsn", count: 2 }, { region: "syd", count: 1 }]);
  });

  it("keeps a Service following the Environment while it has one row in its region", () => {
    expect(replicaValue([{ region: "fsn", count: 2 }], "fsn")).toBe(2);
    expect(replicaValue([{ region: null, count: 2 }], null)).toBe(2);
    expect(replicaValue([{ region: "fsn", count: 2 }, { region: "syd", count: 1 }], "fsn")).toEqual({ fsn: 2, syd: 1 });
    expect(replicaValue([{ region: "syd", count: 1 }], "fsn")).toEqual({ syd: 1 });
  });

  it("goes back to a bare count when a region row is removed", () => {
    const rows = replicaRows({ fsn: 2, syd: 1 }, "fsn").filter(({ region }) => region !== "syd");
    expect(replicaValue(rows, "fsn")).toBe(2);
  });

  it("totals every region, which a Volume's writers count by", () => {
    expect(replicaTotal({ fsn: 2, syd: 1 })).toBe(3);
    expect(replicaTotal(2)).toBe(2);
  });

  it("words a change for Details with the regions it runs in", () => {
    expect(replicaText(2, "fsn")).toBe("fsn 2");
    expect(replicaText({ fsn: 2, syd: 1 }, "fsn")).toBe("fsn 2 · syd 1");
    expect(replicaText(2, null)).toBe("2");
  });
});
