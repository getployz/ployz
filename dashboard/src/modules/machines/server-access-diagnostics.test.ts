import { describe, expect, it } from "vitest";
import { dialTimingFields, shouldLogDialTiming } from "./server-access-diagnostics";

describe("Server Access dial diagnostics", () => {
  it("emits slow and unanswered dials but skips a fast unsampled answer", () => {
    expect(shouldLogDialTiming({ totalMs: 2_001, outcome: "answered", sampled: false })).toBe(true);
    expect(shouldLogDialTiming({ totalMs: 40, outcome: "timed_out", sampled: false })).toBe(true);
    expect(shouldLogDialTiming({ totalMs: 40, outcome: "cancelled", sampled: false })).toBe(true);
    expect(shouldLogDialTiming({ totalMs: 40, outcome: "answered", sampled: false })).toBe(false);
  });

  it("emits no capability or relay address even if they enter the input", () => {
    const fields = dialTimingFields({
      totalMs: 72,
      dialMs: 48,
      setMs: 24,
      outcome: "answered",
      sampled: true,
      capability: "ployz1:private-fixture",
      relayUrl: "https://secret-relay.example.com",
    });
    const logged = JSON.stringify(fields);
    expect(logged).toContain('"dialMs":48');
    expect(logged).not.toContain("ployz1:private-fixture");
    expect(logged).not.toContain("secret-relay.example.com");
  });
});
