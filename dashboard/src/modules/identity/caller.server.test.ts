import { describe, expect, it } from "vitest";
import { ployzVia } from "#/modules/identity/caller.server";

describe("ployzVia", () => {
  it.each([
    ["cli", undefined, "cli"],
    ["cli", "cli", "cli"],
    ["cli", "agent", "agent"],
    ["cli", "dashboard", "cli"],
    ["agent", undefined, "agent"],
    ["agent", "cli", "agent"],
    ["agent", "dashboard", "agent"],
    ["agent", "agent", "agent"],
  ] as const)("a %s credential with x-ployz-via %s speaks as %s", (credential, header, via) => {
    const headers = new Headers(header === undefined ? {} : { "x-ployz-via": header });
    expect(ployzVia(credential, headers)).toBe(via);
  });
});
