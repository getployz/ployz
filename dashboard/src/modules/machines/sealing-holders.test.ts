import type { RuntimeWatchView, Sealing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";

import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

import { sealingHolders } from "./sealing-holders";

const KEY = `${"A".repeat(43)}=`;
const SEALING = {
  held: { kind: "held", public_key: KEY },
  needs_access: { kind: "needs_access" },
  awaits_recovery: { kind: "awaits_recovery", public_key: KEY },
  unknown: null,
} as const satisfies Record<string, Sealing | null>;

const frame = (servers: Record<string, keyof typeof SEALING>): RuntimeWatchView =>
  runtimeWatchFrameFixture({
    machines: Object.entries(servers).map(([id, sealing]) => {
      const machine = runtimeWatchMachineFixture(id, id);
      return runtimeWatchMachineObservationFixture({
        machine: { ...machine, runtime: { ...machine.runtime, sealing: SEALING[sealing] } },
      });
    }),
  });

describe("sealingHolders", () => {
  it("splits Servers into those holding the Sealing Key and those still lacking it", () => {
    expect(sealingHolders(frame({ m1: "held", m2: "needs_access", m3: "awaits_recovery", m4: "held" }))).toEqual({
      held: ["m1", "m4"],
      lacking: ["m2", "m3"],
    });
  });

  it("leaves out a Server that doesn't report sealing", () => {
    expect(sealingHolders(frame({ m1: "unknown", m2: "held" }))).toEqual({ held: ["m2"], lacking: [] });
  });

  it("is empty for a frame with no Servers", () => {
    expect(sealingHolders(frame({}))).toEqual({ held: [], lacking: [] });
  });
});
