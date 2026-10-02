import { describe, expect, it } from "vitest";
import { liveNodes, presentRow } from "./store-branches";

describe("Rows over the Store", () => {
  it("words a row by node and setting", () => {
    expect(presentRow({ row: "web.image", from: "web:2", into: "web:1" }))
      .toMatchObject({ node: "web", label: "Container image", before: "web:1", after: "web:2" });
    expect(presentRow({ row: "web.env.API_KEY", from: { secret: true }, into: null }))
      .toMatchObject({ node: "web", label: "API_KEY", before: "", after: "hidden" });
    expect(presentRow({ row: "cache", from: "cache", into: null })).toMatchObject({ node: "cache", label: "New", after: "" });
    expect(presentRow({ row: "volumes.data", from: null, into: null })).toMatchObject({ lineageId: "volumes.data", node: "data", label: "New" });
    expect(presentRow({ row: "volumes.data.size", from: "5GB", into: "1GB" })).toMatchObject({ lineageId: "volumes.data", node: "data" });
  });
});

describe("Live Nodes over the Store", () => {
  it("links each to the Services the Store says read it", () => {
    const node = { name: "db", owner: "shop-production", data: true, used_by: ["web"] };
    expect(liveNodes([node], [{ id: "w", name: "web" }, { id: "a", name: "api" }])).toEqual([{ ...node, usedBy: ["w"] }]);
  });
});
