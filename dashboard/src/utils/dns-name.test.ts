import { describe, expect, it } from "vitest";
import { dnsName } from "#/utils/dns-name";

describe("dnsName", () => {
  it("converts accents and drops other scripts, previewing the saved name", () => {
    expect(dnsName("Données 日本")).toEqual({ name: "donnees", preview: "donnees", error: null });
    expect(dnsName("Rédis Cache 東京")).toEqual({ name: "redis-cache", preview: "redis-cache", error: null });
  });

  it("shows no preview when the name is saved as typed", () => {
    expect(dnsName("my-cache")).toEqual({ name: "my-cache", preview: null, error: null });
  });

  it("blocks a name with nothing left after conversion", () => {
    expect(dnsName("日本")).toEqual({ name: "", preview: null, error: "Use letters a to z or digits." });
  });

  it("leaves a blank name empty without an error", () => {
    expect(dnsName("   ")).toEqual({ name: "", preview: null, error: null });
  });

  it("trims long input to 63 characters without a trailing hyphen", () => {
    const long = dnsName("a".repeat(70));
    expect(long.name).toBe("a".repeat(63));
    expect(long.preview).toBe("a".repeat(63));
    expect(dnsName(`${"a".repeat(62)} b`).name).toBe("a".repeat(62));
  });
});
