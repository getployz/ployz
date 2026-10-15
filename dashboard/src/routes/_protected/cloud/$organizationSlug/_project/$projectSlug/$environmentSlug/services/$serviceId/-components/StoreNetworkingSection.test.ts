import type { DomainRow } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { defaultPortHint, freePrefix, withDomainAdded, withGeneratedDomain } from "./StoreNetworkingSection";

describe("defaultPortHint", () => {
  it("names Ployz's default PORT when a domain routes to PORT and the user set none", () => {
    expect(defaultPortHint(undefined, [{ port: null }])).toBe(8080);
    expect(defaultPortHint(null, [{ port: 3000 }, { port: null }])).toBe(8080);
  });

  it("says nothing once the user set PORT, every domain has its own port, or there is no domain", () => {
    expect(defaultPortHint("80", [{ port: null }])).toBeNull();
    expect(defaultPortHint(undefined, [{ port: 80 }])).toBeNull();
    expect(defaultPortHint(undefined, [])).toBeNull();
  });
});

describe("freePrefix", () => {
  it("takes the base, else the first numbered one no generated domain has", () => {
    expect(freePrefix("web", [])).toBe("web");
    expect(freePrefix("web", ["web", "web-2"])).toBe("web-3");
    expect(freePrefix("a".repeat(63), ["a".repeat(63)])).toBe(`${"a".repeat(61)}-2`);
  });
});

const live = { status: "ready", reason: null, action: null, service: "web" } as const;
const generated: DomainRow = { ...live, kind: "generated", prefix: "web", hostname: "web.c.ployz.dev", port: null };
const custom: DomainRow = { ...live, kind: "custom", hostname: "shop.example.com", port: 80 };
const add = { service: "web", privateDns: "web-dns", hostname: null, prefix: null, port: null };

describe("withDomainAdded", () => {
  it("adds a custom or generated domain, staged until the next Deploy", () => {
    expect(withDomainAdded([], { ...add, hostname: "a.example.com", port: 3000 })).toEqual([{
      kind: "custom", hostname: "a.example.com", service: "web", port: 3000,
      status: "setting_up", reason: "Live after your next deploy", action: { type: "deploy" },
    }]);
    expect(withDomainAdded([], add)).toMatchObject([{ kind: "generated", prefix: "web-dns", hostname: null, status: "setting_up" }]);
    expect(withDomainAdded([generated], { ...add, prefix: "api" })).toMatchObject([generated, { kind: "generated", prefix: "api" }]);
  });

  it("retargets one it has in place, and leaves a generated one alone without a port", () => {
    expect(withDomainAdded([custom], { ...add, hostname: "shop.example.com", port: 81 })).toMatchObject([{ hostname: "shop.example.com", port: 81, status: "setting_up" }]);
    expect(withDomainAdded([generated], add)).toEqual([generated]);
  });
});

describe("withGeneratedDomain", () => {
  it("renames and retargets the generated domain, its hostname under the same Cluster Domain", () => {
    expect(withGeneratedDomain([generated, custom], "web", "app", 9000)).toEqual([
      { ...generated, prefix: "app", hostname: "app.c.ployz.dev", port: 9000 }, custom,
    ]);
  });
});
