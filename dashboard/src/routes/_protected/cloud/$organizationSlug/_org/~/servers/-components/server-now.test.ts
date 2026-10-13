import { describe, expect, it } from "vitest";
import type { ServerMetrics } from "#/modules/metrics/metrics-model";
import { serverNow } from "./server-now";

const TO = 1_000_000;

const empty: ServerMetrics = {
  availability: { type: "normal" },
  window: { from: TO - 300_000, to: TO },
  stepMs: 10_000,
  cpu: [],
  memoryUsed: [],
  memoryTotal: [],
  interfaces: [],
  disk: [],
  gaps: [],
};

const recent = TO - 10_000;

describe("serverNow", () => {
  it("reads the newest sample of each meter", () => {
    const now = serverNow({
      ...empty,
      cpu: [{ at: recent - 10_000, value: 0.9 }, { at: recent, value: 0.37 }],
      memoryUsed: [{ at: recent - 10_000, value: 10 }, { at: recent, value: 16 }],
      memoryTotal: [{ at: recent - 10_000, value: 64 }, { at: recent, value: 64 }],
      disk: [{ at: recent - 60_000, total: 100, free: 90 }, { at: recent, total: 100, free: 77 }],
    });
    expect(now).toEqual({ type: "meters", cpu: 0.37, memory: { used: 16, total: 64 }, disk: { used: 23, total: 100 } });
  });

  it("leaves a meter the Server sent no reading of empty, not zero", () => {
    expect(serverNow({ ...empty, cpu: [{ at: recent, value: 0.5 }] })).toEqual({ type: "meters", cpu: 0.5, memory: null, disk: null });
    expect(serverNow({ ...empty, cpu: [{ at: recent, value: 0.5 }], disk: [{ at: recent, total: 0, free: 0 }] })).toMatchObject({ disk: null });
  });

  it("leaves out a reading too old to call current", () => {
    const now = serverNow({
      ...empty,
      cpu: [{ at: TO - 280_000, value: 0.37 }],
      memoryUsed: [{ at: TO - 40_000, value: 16 }],
      memoryTotal: [{ at: TO - 40_000, value: 64 }],
      // Disk is sampled once a minute, so a two-minute-old reading is still its latest.
      disk: [{ at: TO - 120_000, total: 100, free: 77 }],
      gaps: [{ from: TO - 270_000, to: TO, reason: "not_sampled" }],
    });
    expect(now).toEqual({ type: "meters", cpu: null, memory: null, disk: { used: 23, total: 100 } });
  });

  it("says there are no readings when every reading is old", () => {
    expect(serverNow({ ...empty, cpu: [{ at: TO - 280_000, value: 0.37 }], disk: [{ at: TO - 280_000, total: 100, free: 77 }] }))
      .toEqual({ type: "no_readings" });
  });

  it("scales what counts as current with a broader read's buckets", () => {
    const hourly = { ...empty, window: { from: TO - 3_600_000, to: TO }, stepMs: 60_000 };
    expect(serverNow({ ...hourly, cpu: [{ at: TO - 120_000, value: 0.37 }] })).toMatchObject({ cpu: 0.37 });
    expect(serverNow({ ...hourly, cpu: [{ at: TO - 240_000, value: 0.37 }] })).toEqual({ type: "no_readings" });
  });

  it("names why a Server has no meters", () => {
    const ref = { id: "2".repeat(32), name: "syd-1" };
    expect(serverNow({ ...empty, availability: { type: "needs_upgrade", servers: [ref] } })).toEqual({ type: "needs_upgrade" });
    expect(serverNow({ ...empty, availability: { type: "unanswered", servers: [ref], needsUpgrade: [] } })).toEqual({ type: "unanswered" });
    expect(serverNow({ ...empty, availability: { type: "offline" } })).toEqual({ type: "unanswered" });
    expect(serverNow({ ...empty, availability: { type: "no_readings" } })).toEqual({ type: "no_readings" });
  });
});
