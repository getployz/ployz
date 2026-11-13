import { describe, expect, it } from "vitest";
import { joinCode } from "#/modules/machines/join-code.server";

describe("join code", () => {
  it("matches the code the Rust engine prints for the same Machine ID and key", () => {
    // The same vector is asserted in ployz-core's authority/join.rs.
    expect(joinCode("0123456789abcdef0123456789abcdef", Array.from({ length: 32 }, (_, index) => index))).toBe("jhwpvddkhc7z72egn3lg");
  });
});
