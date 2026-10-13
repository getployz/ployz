import { describe, expect, it } from "vitest";
import type { ServerMetrics } from "#/modules/metrics/metrics-model";
import { serverNow } from "./server-now";

const empty: ServerMetrics = {
  availability: { type: "normal" },
  window: { from: 0, to: 300_000 },
  stepMs: 10_000,
  cpu: [],
  memoryUsed: [],
  memoryTotal: [],
  interfaces: [],
  disk: [],
  gaps: [],
};

describe("serverNow", () => {
  it("reads the newest sample of each meter", () => {
    const now = serverNow({
      ...empty,
      cpu: [{ at: 1, value: 0.9 }, { at: 2, value: 0.37 }],
      memoryUsed: [{ at: 1, value: 10 }, { at: 2, value: 16 }],
      memoryTotal: [{ at: 1, value: 64 }, { at: 2, value: 64 }],
      disk: [{ at: 1, total: 100, free: 90 }, { at: 2, total: 100, free: 77 }],
    });
    expect(now).toEqual({ type: "meters", cpu: 0.37, memory: 0.25, disk: 0.23 });
  });

  it("leaves a meter the Server sent no reading of empty, not zero", () => {
    expect(serverNow({ ...empty, cpu: [{ at: 1, value: 0.5 }] })).toEqual({ type: "meters", cpu: 0.5, memory: null, disk: null });
    expect(serverNow({ ...empty, disk: [{ at: 1, total: 0, free: 0 }] })).toMatchObject({ disk: null });
  });

  it("names why a Server has no meters", () => {
    const ref = { id: "2".repeat(32), name: "syd-1" };
    expect(serverNow({ ...empty, availability: { type: "needs_upgrade", servers: [ref] } })).toEqual({ type: "needs_upgrade" });
    expect(serverNow({ ...empty, availability: { type: "unanswered", servers: [ref], needsUpgrade: [] } })).toEqual({ type: "unanswered" });
    expect(serverNow({ ...empty, availability: { type: "offline" } })).toEqual({ type: "unanswered" });
    expect(serverNow({ ...empty, availability: { type: "no_readings" } })).toEqual({ type: "no_readings" });
  });
});
