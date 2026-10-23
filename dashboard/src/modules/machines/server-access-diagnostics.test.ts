import { describe, expect, it, vi } from "vitest";
import { dialCorrelation, dialTimingFields, nativeDialTimingFields, shouldLogDialTiming } from "./server-access-diagnostics";

describe("Server Access dial diagnostics", () => {
  it("emits slow and unanswered dials but skips a fast unsampled answer", () => {
    const base = { stage: "cloud_dial", sampled: false, correlation: "0011223344556677" } as const;
    expect(shouldLogDialTiming({ ...base, totalMs: 2_000, outcome: "answered" })).toBe(true);
    expect(shouldLogDialTiming({ ...base, totalMs: 1_999, outcome: "answered" })).toBe(false);
    expect(shouldLogDialTiming({ ...base, totalMs: 40, outcome: "timed_out" })).toBe(true);
    expect(shouldLogDialTiming({ ...base, totalMs: 40, outcome: "cancelled" })).toBe(true);
    expect(shouldLogDialTiming({ ...base, totalMs: 40, outcome: "refused" })).toBe(true);
    expect(shouldLogDialTiming({ ...base, totalMs: 40, outcome: "unanswered" })).toBe(true);
    expect(shouldLogDialTiming({ ...base, totalMs: 40, outcome: "answered" })).toBe(false);
  });

  it("shares a random correlation across native, dial, and Set fields", () => {
    const correlation = "0011223344556677";
    const knownIds = ["machine-1", "organization-1", "credential-1"];
    const native = nativeDialTimingFields({ totalMs: 12, outcome: "answered", pathKind: "unknown", correlation });
    const dial = dialTimingFields({ stage: "cloud_dial", totalMs: 12, outcome: "answered", sampled: true, correlation });
    const set = dialTimingFields({ stage: "set_management_client", totalMs: 12, outcome: "answered", sampled: true, correlation });
    for (const fields of [native, dial, set]) {
      expect(fields.correlation).toBe(correlation);
      for (const id of knownIds) expect(fields.correlation).not.toBe(id);
      expect(Object.keys(fields)).not.toContain("dialMs");
      expect(Object.keys(fields)).not.toContain("setMs");
    }
  });

  it("generates an opaque correlation with 8 random bytes", () => {
    const first = dialCorrelation();
    const second = dialCorrelation();
    expect(first).toMatch(/^[0-9a-f]{16}$/);
    expect(second).toMatch(/^[0-9a-f]{16}$/);
    expect(first).not.toBe(second);
    for (const id of ["machine-1", "organization-1", "credential-1"]) expect(first).not.toBe(id);
  });

  it("names the deadline that stopped a native dial", () => {
    const cancelled = { totalMs: 10_000, outcome: "cancelled", stoppedBy: "caller", pathKind: "unknown", correlation: "0011223344556677" } as const;
    expect(nativeDialTimingFields(cancelled, "timed_out").stoppedBy).toBe("cloud_deadline");
    expect(nativeDialTimingFields(cancelled, "cancelled").stoppedBy).toBe("caller");
    const sdkDeadline = { ...cancelled, outcome: "timed_out", stoppedBy: "sdk_deadline" } as const;
    expect(nativeDialTimingFields(sdkDeadline, "timed_out").stoppedBy).toBe("sdk_deadline");
  });

  it.each(["0", "10001", "nope", "1.5", "9007199254740992"])("rejects invalid sample rate %s", async (rate) => {
    vi.resetModules();
    vi.stubEnv("PLOYZ_DIAL_SAMPLE_N", rate);
    const { sampleDialTiming } = await import("./server-access-diagnostics");
    expect(Array.from({ length: 99 }, sampleDialTiming)).toEqual(Array(99).fill(false));
    expect(sampleDialTiming()).toBe(true);
    vi.unstubAllEnvs();
  });

  it("samples every dial when the rate is one", async () => {
    vi.resetModules();
    vi.stubEnv("PLOYZ_DIAL_SAMPLE_N", "1");
    const { sampleDialTiming } = await import("./server-access-diagnostics");
    expect(sampleDialTiming()).toBe(true);
    expect(sampleDialTiming()).toBe(true);
    vi.unstubAllEnvs();
  });

  it("emits no capability or relay address even if they enter the input", () => {
    const fixture = {
      totalMs: 72,
      stage: "set_management_client",
      outcome: "answered",
      sampled: true,
      correlation: "0011223344556677",
      capability: "ployz1:private-fixture",
      relayUrl: "https://secret-relay.example.com",
    } as const;
    const fields = dialTimingFields(fixture);
    const logged = JSON.stringify(fields);
    expect(logged).toContain('"totalMs":72');
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
