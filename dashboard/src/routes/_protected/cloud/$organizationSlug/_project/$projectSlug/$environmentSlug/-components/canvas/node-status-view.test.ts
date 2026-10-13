import { describe, expect, it } from "vitest";
import { issuesText } from "./node-status-view";

describe("issuesText", () => {
  it("names the trouble alone, with how many other issues share the card, else counts them", () => {
    expect(issuesText({ count: 1, tone: "warn", monitoring: true, trouble: "OOM" })).toBe("OOM");
    expect(issuesText({ count: 2, tone: "bad", monitoring: true, trouble: "OOM" })).toBe("OOM +1");
    expect(issuesText({ count: 3, tone: "warn", monitoring: false, trouble: null })).toBe("3");
  });
});
