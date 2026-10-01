import { describe, expect, it } from "vitest";
import { defaultPortHint } from "./StoreNetworkingSection";

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
