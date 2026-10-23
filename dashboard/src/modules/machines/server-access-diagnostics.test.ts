import { describe, expect, it } from "vitest";
import { dialTimingFields, nativeDialTimingFields, shouldLogDialTiming } from "./server-access-diagnostics";

describe("Server Access dial diagnostics", () => {
  it("emits slow and unanswered dials but skips a fast unsampled answer", () => {
    expect(shouldLogDialTiming({ stage: "cloud_dial", dialMs: 2_001, totalMs: 2_001, outcome: "answered", sampled: false })).toBe(true);
    expect(shouldLogDialTiming({ stage: "cloud_dial", dialMs: 40, totalMs: 40, outcome: "timed_out", sampled: false })).toBe(true);
    expect(shouldLogDialTiming({ stage: "cloud_dial", dialMs: 40, totalMs: 40, outcome: "cancelled", sampled: false })).toBe(true);
    expect(shouldLogDialTiming({ stage: "cloud_dial", dialMs: 40, totalMs: 40, outcome: "answered", sampled: false })).toBe(false);
  });

  it("emits no capability or relay address even if they enter the input", () => {
    const fixture = {
      totalMs: 72,
      stage: "set_management_client",
      setMs: 24,
      outcome: "answered",
      sampled: true,
      capability: "ployz1:private-fixture",
      relayUrl: "https://secret-relay.example.com",
    } as const;
    const fields = dialTimingFields(fixture);
    const logged = JSON.stringify(fields);
    expect(logged).toContain('"setMs":24');
    expect(logged).not.toContain("ployz1:private-fixture");
    expect(logged).not.toContain("secret-relay.example.com");
    const nativeFixture = {
      totalMs: 72, pathKind: "relayed", outcome: "answered", proofMs: 2,
      capability: "ployz1:private-fixture", relayUrl: "https://secret-relay.example.com",
    } as const;
    const native = nativeDialTimingFields(nativeFixture);
    expect(JSON.stringify(native)).not.toContain("private-fixture");
    expect(JSON.stringify(native)).not.toContain("secret-relay.example.com");
  });
});
