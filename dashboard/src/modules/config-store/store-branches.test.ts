import type { MoveChoice } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { liveNodes, movePicks, presentMoveRow } from "./store-branches";

const variable: MoveChoice = { default: "from", options: ["from", "new", "leave_out"], secret: false };

describe("Save sheet over the Store", () => {
  it("words a Move row by node and setting", () => {
    expect(presentMoveRow({ row: "web.image", conflict: true, from: "web:2", into: "web:1" }))
      .toMatchObject({ node: "web", label: "Container image", before: "web:1", after: "web:2" });
    expect(presentMoveRow({ row: "web.env.API_KEY", conflict: false, choice: { ...variable, secret: true }, from: { secret: true }, into: null }))
      .toMatchObject({ node: "web", label: "API_KEY", before: "", after: "hidden" });
    expect(presentMoveRow({ row: "cache", conflict: false, from: "cache", into: null })).toMatchObject({ node: "cache", label: "New", after: "" });
  });

  it("moves a new node whole, its unticked variables left out, and a kept node's settings one by one", () => {
    expect(movePicks([
      { key: "cache", ticked: true, value: "" },
      { key: "cache.env.A", ticked: false, choice: variable, value: "" },
      { key: "cache.env.B", ticked: true, choice: variable, value: "" },
      { key: "web.image", ticked: true, value: "" },
      { key: "web.replicas", ticked: false, value: "" },
      { key: "web.env.TOKEN", ticked: true, choice: { ...variable, default: "new", secret: true }, option: "new", value: "fresh" },
      { key: "old", ticked: false, value: "" },
      { key: "old.env.C", ticked: true, choice: variable, value: "" },
    ])).toEqual([
      { row: "cache" },
      { row: "cache.env.A", choice: "leave_out" },
      { row: "cache.env.B", choice: "from" },
      { row: "web.image" },
      { row: "web.env.TOKEN", choice: "new", value: "fresh" },
    ]);
  });
});

describe("Live Nodes over the Store", () => {
  it("links each to the Services the Store says read it", () => {
    const node = { name: "db", owner: "shop-production", data: true, used_by: ["web"] };
    expect(liveNodes([node], [{ id: "w", name: "web" }, { id: "a", name: "api" }])).toEqual([{ ...node, usedBy: ["w"] }]);
  });
});
