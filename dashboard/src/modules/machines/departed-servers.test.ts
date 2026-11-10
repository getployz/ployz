import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  runtimeWatchFrameFixture, runtimeWatchMachineFixture, runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { DEPARTED_GRACE_MS, departedServers, runtimeMembershipDigest } from "./departed-servers";

const id = (hex: string) => hex.repeat(32) as MachineId;
const now = new Date("2026-11-10T12:00:00Z");
const old = new Date(now.getTime() - DEPARTED_GRACE_MS);
const recent = new Date(now.getTime() - DEPARTED_GRACE_MS + 1);
const frame = (...listed: readonly (readonly [string, string])[]) => runtimeWatchFrameFixture({
  machines: listed.map(([hex, membership]) =>
    runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(id(hex), `web-${hex}`), membership })),
});

describe("departedServers", () => {
  it("names a Server the Cluster stopped listing once its grace has passed", () => {
    const rows = [{ machineId: id("a"), updatedAt: old }, { machineId: id("b"), updatedAt: old }];
    expect(departedServers(rows, frame(["a", "up"]), now, new Set())).toEqual({ departed: [id("b")], held: [] });
  });

  it("holds a row saved within the grace, or one Cloud's own removal runs on", () => {
    const rows = [{ machineId: id("b"), updatedAt: recent }, { machineId: id("c"), updatedAt: old }];
    expect(departedServers(rows, frame(["a", "up"]), now, new Set([id("c")]))).toEqual({ departed: [], held: [id("b"), id("c")] });
  });

  it("drops nothing on a frame where any Server does not read up, or that lists none", () => {
    const rows = [{ machineId: id("b"), updatedAt: old }];
    expect(departedServers(rows, frame(["a", "up"], ["c", "suspect"]), now, new Set())).toEqual({ departed: [], held: [] });
    expect(departedServers(rows, frame(), now, new Set())).toEqual({ departed: [], held: [] });
  });

  it("drops nothing while a Server's row is incomplete, and reads it again once the frame changes", () => {
    const rows = [{ machineId: id("a"), updatedAt: old }, { machineId: id("b"), updatedAt: old }];
    const partial = { ...frame(["a", "up"]), incomplete_ids: { ...frame().incomplete_ids, machines: [id("b")] } };
    expect(departedServers(rows, partial, now, new Set())).toEqual({ departed: [], held: [] });
    expect(runtimeMembershipDigest(partial)).not.toEqual(runtimeMembershipDigest(frame(["a", "up"])));
  });
});
