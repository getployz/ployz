import { describe, expect, it } from "vitest";
import { cleanupPending } from "./retire-old.queries";

describe("asking again for a Deployment's cleanup", () => {
  const ended = { in_flight: false, ended_at: 1_000 };
  const at = (seconds: number) => seconds * 1000;

  it("goes on while it waits, and stops once it ended", () => {
    expect(cleanupPending({ state: "waiting", left: [], message: null }, ended, at(5_000))).toBe(true);
    expect(cleanupPending({ state: "left_running", left: [], message: null }, ended, at(1_010))).toBe(false);
  });

  it("goes on for a cleanup not there yet while its Deployment runs and for a minute after, then gives up", () => {
    expect(cleanupPending(null, { in_flight: true, ended_at: null }, at(5_000))).toBe(true);
    expect(cleanupPending(undefined, ended, at(1_030))).toBe(true);
    expect(cleanupPending(null, ended, at(1_060))).toBe(false);
  });
});
