import { describe, expect, it } from "vitest";
import { blankPortTarget, defaultPortHint, freePrefix } from "./StoreNetworkingSection";

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

describe("blankPortTarget", () => {
  it("is the Service's PORT, else the one Ployz sets", () => {
    expect(blankPortTarget("3000")).toBe(3000);
    expect(blankPortTarget(3000)).toBe(3000);
    expect(blankPortTarget(undefined)).toBe(8080);
    expect(blankPortTarget(null)).toBe(8080);
  });

  it("is unknown when PORT holds something other than a port", () => {
    expect(blankPortTarget("${{shared.PORT}}")).toBeNull();
    expect(blankPortTarget("70000")).toBeNull();
  });
});

describe("freePrefix", () => {
  it("takes the base, else the first numbered one no generated domain has", () => {
    expect(freePrefix("web", [])).toBe("web");
    expect(freePrefix("web", ["web", "web-2"])).toBe("web-3");
    expect(freePrefix("a".repeat(63), ["a".repeat(63)])).toBe(`${"a".repeat(61)}-2`);
  });
});
