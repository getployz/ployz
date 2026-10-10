import { describe, expect, it } from "vitest";
import type { JsonValue, RowId } from "@ployz/sdk";
import { liveNodes, presentRow } from "./store-branches";

describe("Rows over the Store", () => {
  it("words a row by node and setting", () => {
    const row = (node: string, name: string | null, value: JsonValue) =>
      ({ row: `${node}:${name}` as RowId, node, kind: node.startsWith("volumes.") ? "volume" as const : "service" as const, name, value });
    expect(presentRow(row("web", "source", { type: "image", image: "web:2", credentials: false }))).toEqual({ node: "web", label: "Source", after: "web:2" });
    expect(presentRow(row("web", "env.API_KEY", { secret: true }))).toEqual({ node: "web", label: "API_KEY", after: "hidden" });
    expect(presentRow(row("cache", null, "cache"))).toEqual({ node: "cache", label: "New", after: "" });
    expect(presentRow(row("volumes.data", null, null))).toEqual({ node: "data", label: "New", after: "" });
  });

  it("summarizes Config files by explicit row identity without revealing their text or metadata", () => {
    const row = { row: "c:files/app.conf.bak" as RowId, node: "configs.app-settings", kind: "config" as const, name: "files.nested/app.conf.bak" };
    const value = { content: "private", mode: "0440", uid: 1000, gid: 1000 };
    expect(presentRow({ ...row, value })).toEqual({ node: "app-settings", label: "nested/app.conf.bak", after: "File" });
    expect(presentRow({ ...row, value: null })).toEqual({ node: "app-settings", label: "nested/app.conf.bak", after: "" });
    expect(presentRow({ ...row, node: "web", kind: "service", value }).after).toBe(JSON.stringify(value));
  });
});

describe("Live Nodes over the Store", () => {
  it("links each to the Services the Store says read it", () => {
    const node = { name: "db", owner: "shop-production", data: true, used_by: ["web"] };
    expect(liveNodes([node], [{ id: "w", name: "web" }, { id: "a", name: "api" }])).toEqual([{ ...node, usedBy: ["w"] }]);
  });
});
