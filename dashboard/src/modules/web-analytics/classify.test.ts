import { describe, expect, it } from "vitest";
import { classify } from "#/modules/web-analytics/classify";

const browser = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const page = { method: "GET", status: 200, ua: browser, contentType: "text/html; charset=utf-8", secPurpose: null };

describe("classify", () => {
  it("counts a browser's successful html GET as a pageview", () => {
    expect(classify(page)).toEqual({ kind: "pageview", botReason: null });
  });

  it("names the crawler that matched", () => {
    expect(classify({ ...page, ua: "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)" }))
      .toEqual({ kind: "bot", botReason: "google" });
  });

  it("ranks bot above every other trait", () => {
    expect(classify({ ...page, method: "POST", ua: "curl/8.5.0" }).kind).toBe("bot");
  });

  it.each([
    ["a POST", { method: "POST" }],
    ["a redirect", { status: 301 }],
    ["a 404", { status: 404 }],
    ["json", { contentType: "application/json" }],
    ["no content type", { contentType: null }],
    ["a prefetch", { secPurpose: "prefetch" }],
    ["a prerender", { secPurpose: "prefetch;prerender" }],
  ])("counts %s as other", (_name, change) => {
    expect(classify({ ...page, ...change })).toEqual({ kind: "other", botReason: null });
  });
});
