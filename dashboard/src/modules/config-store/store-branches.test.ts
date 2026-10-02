import { describe, expect, it } from "vitest";
import type { JsonValue, RowId } from "@ployz/sdk";
import { liveNodes, presentRow, rowPath } from "./store-branches";

describe("Rows over the Store", () => {
  it("words a row by node and setting", () => {
    const row = (node: string, name: string | null, value: JsonValue) => ({ row: `${node}:${name}` as RowId, node, name, value });
    expect(presentRow(row("web", "image", "web:2"))).toEqual({ node: "web", label: "Container image", after: "web:2" });
    expect(presentRow(row("web", "env.API_KEY", { secret: true }))).toEqual({ node: "web", label: "API_KEY", after: "hidden" });
    expect(presentRow(row("cache", null, "cache"))).toEqual({ node: "cache", label: "New", after: "" });
    expect(presentRow(row("volumes.data", null, null))).toEqual({ node: "data", label: "New", after: "" });
    expect(rowPath(row("volumes.data", "size", null))).toBe("volumes.data.size");
    expect(rowPath(row("cache", null, null))).toBe("cache");
  });
});

describe("Live Nodes over the Store", () => {
  it("links each to the Services the Store says read it", () => {
    const node = { name: "db", owner: "shop-production", data: true, used_by: ["web"] };
    expect(liveNodes([node], [{ id: "w", name: "web" }, { id: "a", name: "api" }])).toEqual([{ ...node, usedBy: ["w"] }]);
  });
});
