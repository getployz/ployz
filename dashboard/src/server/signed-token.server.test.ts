import { describe, expect, it } from "vitest";
import { openToken, signature, signToken } from "#/server/signed-token.server";

const SECRET = "app-encryption-secret-at-least-32-characters";

// Outputs of the code this replaced, which links and claim secrets already issued must keep verifying against.
describe("signed token", () => {
  it("signs byte for byte as the claim, GitHub connect and unsubscribe code did", () => {
    expect(signature("ployz:claim:v1", SECRET, "org-id")).toBe("5Jc4lkki_-FfaLJekA1jWhf9vFmUCf-jFLcAYqxLcr0");
    expect(signToken("ployz:github-connect-state:v1", SECRET, "eyJvcmdhbml6YXRpb25JZCI6Im8iLCJleHBpcmVzQXQiOjF9")).toBe(
      "eyJvcmdhbml6YXRpb25JZCI6Im8iLCJleHBpcmVzQXQiOjF9.dqQhDomT3PZa4JaGJ6zXXJ_EgiF8IcpdAY7Dj1023yQ",
    );
    expect(signToken("ployz:unsubscribe:v1", SECRET, "user-1.uptime")).toBe("user-1.uptime.uMct-xxWhrHHXMyYI1B12FqqI0Y-gJwGg9qGj3P1GRY");
  });

  it("opens only a token signed for the same purpose and secret", () => {
    const token = signToken("ployz:unsubscribe:v1", SECRET, "user-1.uptime");
    expect(openToken("ployz:unsubscribe:v1", SECRET, token)).toBe("user-1.uptime");
    expect(openToken("ployz:github-connect-state:v1", SECRET, token)).toBeNull();
    expect(openToken("ployz:unsubscribe:v1", `${SECRET}x`, token)).toBeNull();
    expect(openToken("ployz:unsubscribe:v1", SECRET, token.replace("user-1", "user-2"))).toBeNull();
    expect(openToken("ployz:unsubscribe:v1", SECRET, "no-dot")).toBeNull();
  });
});
