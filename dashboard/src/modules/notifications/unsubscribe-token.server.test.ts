import { Option } from "effect";
import { describe, expect, it } from "vitest";
import { signUnsubscribeToken, verifyUnsubscribeToken } from "#/modules/notifications/unsubscribe-token.server";

const SECRET = "app-encryption-secret-at-least-32-characters";
const USER = "00000000-0000-4000-8000-00000000e203";
const token = signUnsubscribeToken(SECRET, { userId: USER, type: "uptime" });

describe("unsubscribe token", () => {
  it("names the user and type it was signed for", () => {
    expect(verifyUnsubscribeToken(SECRET, token)).toEqual(Option.some({ userId: USER, type: "uptime" }));
  });

  it("refuses a token signed with another secret", () => {
    expect(verifyUnsubscribeToken("another-secret-at-least-32-characters-long", token)).toEqual(Option.none());
  });

  it("refuses a token edited to another type or user", () => {
    expect(verifyUnsubscribeToken(SECRET, token.replace(".uptime.", ".deploy-failed."))).toEqual(Option.none());
    expect(verifyUnsubscribeToken(SECRET, token.replace(USER, "00000000-0000-4000-8000-00000000e204"))).toEqual(Option.none());
  });

  it("refuses a malformed token or an unknown type", () => {
    expect(verifyUnsubscribeToken(SECRET, "")).toEqual(Option.none());
    expect(verifyUnsubscribeToken(SECRET, `${token}.extra`)).toEqual(Option.none());
    const unknown = signUnsubscribeToken(SECRET, { userId: USER, type: "uptime" }).replace(".uptime.", ".billing.");
    expect(verifyUnsubscribeToken(SECRET, unknown)).toEqual(Option.none());
  });
});
