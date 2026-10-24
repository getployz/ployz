import type { RuntimeWatchView } from "@ployz/sdk";
import { describe, expect, it } from "vitest";

import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

import { onSealingGap } from "./sealing-gap";

const KEY = `${"A".repeat(43)}=`;
const SEALING = {
  held: { kind: "held", public_key: KEY },
  needs_access: { kind: "needs_access" },
  awaits_recovery: { kind: "awaits_recovery", public_key: KEY },
  unknown: null,
} as const;

const frame = (servers: Record<string, keyof typeof SEALING>): RuntimeWatchView =>
  runtimeWatchFrameFixture({
    machines: Object.entries(servers).map(([id, sealing]) => {
      const machine = runtimeWatchMachineFixture(id, id);
      return runtimeWatchMachineObservationFixture({
        machine: { ...machine, runtime: { ...machine.runtime, sealing: SEALING[sealing] } },
      });
    }),
  });

async function gaps(frames: RuntimeWatchView[]): Promise<number> {
  let count = 0;
  async function* source() {
    yield* frames;
  }
  for await (const _ of onSealingGap(source(), () => count++)) void _;
  return count;
}

describe("onSealingGap", () => {
  it("calls once when a Server recovers the Sealing Key while another still lacks it", async () => {
    expect(await gaps([
      frame({ m5: "needs_access", m7: "awaits_recovery" }),
      frame({ m5: "needs_access", m7: "held" }),
      frame({ m5: "needs_access", m7: "held" }),
    ])).toBe(1);
  });

  it("calls on the first frame when it already shows a Server holding the key and one lacking it", async () => {
    expect(await gaps([frame({ m5: "awaits_recovery", m7: "held" })])).toBe(1);
  });

  it("stays quiet once the key reaches every Server", async () => {
    expect(await gaps([
      frame({ m5: "needs_access", m7: "held" }),
      frame({ m5: "held", m7: "held" }),
      frame({ m5: "held", m7: "held" }),
    ])).toBe(1);
  });

  it("calls again when the Servers holding or lacking the key change", async () => {
    expect(await gaps([
      frame({ m5: "needs_access", m6: "needs_access", m7: "held" }),
      frame({ m5: "held", m6: "needs_access", m7: "held" }),
      frame({ m5: "held", m6: "needs_access", m7: "held", m8: "needs_access" }),
    ])).toBe(3);
  });

  it.each([
    ["no Server holds the key", { m5: "needs_access", m7: "awaits_recovery" }],
    ["every Server holds the key", { m5: "held", m7: "held" }],
    ["the other Server doesn't report sealing", { m5: "unknown", m7: "held" }],
  ] as const)("stays quiet when %s", async (_, servers) => {
    expect(await gaps([frame(servers), frame(servers)])).toBe(0);
  });

  it("passes every frame through", async () => {
    const frames = [frame({ m5: "needs_access", m7: "held" }), frame({ m5: "held", m7: "held" })];
    const seen: RuntimeWatchView[] = [];
    async function* source() {
      yield* frames;
    }
    for await (const next of onSealingGap(source(), () => {})) seen.push(next);
    expect(seen).toEqual(frames);
  });
});
