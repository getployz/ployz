import { describe, expect, it } from "vitest";
import type { JsonValue, RowId } from "@ployz/sdk";
import { liveNodes, presentRow, sourceChange, sourceText } from "./store-branches";

describe("Rows over the Store", () => {
  it("words a row by node and setting", () => {
    const row = (node: string, name: string | null, value: JsonValue) =>
      ({ row: `${node}:${name}` as RowId, node, kind: node.startsWith("volumes.") ? "volume" as const : "service" as const, name, value });
    expect(presentRow(row("web", "source", { type: "image", image: "web:2", credentials: { type: "none" } }))).toEqual({ node: "web", label: "Source", after: "web:2" });
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

describe("A Service's Source in words", () => {
  const image = (ref: string, credentials: JsonValue = { type: "none" }) => ({ type: "image", image: ref, credentials });
  const git = (branch: JsonValue, rootDir = "/") => ({ type: "git", repository: "acme/web", rootDir, branch });
  const main = { type: "connected", name: "main" };

  it("words each kind the Store stores", () => {
    expect(sourceText(image("ghcr.io/acme/api:1.4"))).toBe("ghcr.io/acme/api:1.4");
    expect(sourceText(image("ghcr.io/acme/api:1.4", { type: "configured", credentialId: "c1" }))).toBe("ghcr.io/acme/api:1.4 with credentials");
    expect(sourceText(git(main))).toBe("acme/web · main");
    expect(sourceText(git(main, "apps/web"))).toBe("acme/web · main in apps/web");
    expect(sourceText(git({ type: "disconnected", previousName: "main" }))).toBe("acme/web · no branch");
    expect(sourceText({ type: "empty", rootDir: "/" })).toBe("None");
    expect(sourceText(null)).toBe("");
  });

  it("words a change row's cell, which holds credentials by presence and no branch", () => {
    expect(sourceText(image("ghcr.io/acme/api:1.4", false))).toBe("ghcr.io/acme/api:1.4");
    expect(sourceText(image("ghcr.io/acme/api:1.4", true))).toBe("ghcr.io/acme/api:1.4 with credentials");
    expect(sourceText({ type: "git", repository: "acme/web", rootDir: "apps/web" })).toBe("acme/web in apps/web");
    expect(sourceChange(image("traefik/whoami:v1.10.3", false), image("traefik/whoami:v1.11.0", false))).toEqual(["traefik/whoami:v1.10.3", "v1.11.0"]);
  });

  it("repeats only the tag or branch that changed", () => {
    expect(sourceChange(image("ghcr.io/acme/api:1.4"), image("ghcr.io/acme/api:1.5"))).toEqual(["ghcr.io/acme/api:1.4", "1.5"]);
    expect(sourceChange(image("localhost:5000/api:1.4"), image("localhost:5000/api:1.5"))).toEqual(["localhost:5000/api:1.4", "1.5"]);
    expect(sourceChange(git(main), git({ type: "connected", name: "release" }))).toEqual(["acme/web · main", "release"]);
  });

  it("words both whole when more than the tag or branch changed", () => {
    expect(sourceChange(image("ghcr.io/acme/api:1.4"), image("ghcr.io/acme/web:1.5"))).toEqual(["ghcr.io/acme/api:1.4", "ghcr.io/acme/web:1.5"]);
    expect(sourceChange(image("ghcr.io/acme/api:1.4"), image("ghcr.io/acme/api:1.4", { type: "configured", credentialId: "c1" })))
      .toEqual(["ghcr.io/acme/api:1.4", "ghcr.io/acme/api:1.4 with credentials"]);
    expect(sourceChange(git(main), git(main, "apps/web"))).toEqual(["acme/web · main", "acme/web · main in apps/web"]);
    expect(sourceChange(image("api:1.4"), git(main))).toEqual(["api:1.4", "acme/web · main"]);
    expect(sourceChange(null, image("api:1.4"))).toEqual(["", "api:1.4"]);
  });
});

describe("Live Nodes over the Store", () => {
  it("links each to the Services the Store says read it", () => {
    const node = { name: "db", owner: "shop-production", data: true, used_by: ["web"] };
    expect(liveNodes([node], [{ id: "w", name: "web" }, { id: "a", name: "api" }])).toEqual([{ ...node, usedBy: ["w"] }]);
  });
});
