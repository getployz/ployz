import { describe, expect, it } from "vitest";
import { botsAmong, isBotUserAgent } from "#/modules/web-analytics/classify";

const browser = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const googlebot = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

describe("bot user agents", () => {
  it("takes a browser for a person and a crawler or program for a bot", () => {
    expect([browser, googlebot, "curl/8.5.0", ""].map(isBotUserAgent)).toEqual([false, true, true, false]);
  });

  it("picks the bots out of a window's distinct agents, keeping what rode along with each", () => {
    const agents = [{ ua: browser, hash: "1" }, { ua: googlebot, hash: "2" }, { ua: "curl/8.5.0", hash: "3" }];
    expect(botsAmong(agents).map(({ hash }) => hash)).toEqual(["2", "3"]);
    expect(botsAmong(agents).map(({ hash }) => hash)).toEqual(["2", "3"]);
  });
});
