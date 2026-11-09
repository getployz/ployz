import type { ContainerObservation, ServiceId } from "@ployz/sdk";
import { expect, it } from "vitest";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { CLAIM_WORK_MS, claimOutcome, REFUSED_RESTARTS_MS, routersOn, TAILSCALE } from "./private-access";

const frame = (...routers: (readonly [string, ContainerObservation["runtime"]])[]) =>
  runtimeWatchFrameFixture({
    services: [{
      identity: TAILSCALE,
      service_id: "t".repeat(32) as ServiceId,
      containers: routers.map(([machine, runtime], index) => ({ ...runtimeWatchContainerFixture(machine, `router-${index}`), runtime })),
      hook_containers: [],
    }],
  });

it("calls a router Tailscale gave up on stopped rather than running, so the card never shows it Connected", () => {
  const routers = routersOn(frame(
    ["m1", { state: "running", health: "none" }],
    ["m2", { state: "exited", code: 0 }],
    ["m3", { state: "restarting" }],
    ["m4", { state: "removing" }],
  ));
  expect(Object.fromEntries(routers)).toEqual({ m1: "running", m2: "stopped", m3: "stopped" });
});

it("calls a Server running if any of its routers runs, whatever order they are listed in", () => {
  expect(routersOn(frame(["m1", { state: "running", health: "none" }], ["m1", { state: "exited", code: 1 }])).get("m1")).toBe("running");
  expect(routersOn(frame(["m1", { state: "exited", code: 1 }], ["m1", { state: "running", health: "none" }])).get("m1")).toBe("running");
});

const NOW = Date.parse("2026-11-09T12:00:00Z");
const CLAIMED = { at: new Date(NOW - 60_000).toISOString(), before: ["router-old"] };
const LAPSED = { ...CLAIMED, at: new Date(NOW - CLAIM_WORK_MS - 1).toISOString() };
const SETTLED = REFUSED_RESTARTS_MS + 1;

const placed = (machine: string, id: string, ageMs: number, runtime: ContainerObservation["runtime"] = { state: "running", health: "none" }) =>
  ({ ...runtimeWatchContainerFixture(machine, id), created_at_unix_nanos: (NOW - ageMs) * 1e6, runtime });

const cluster = (machines: readonly (readonly [string, string])[], ...routers: ContainerObservation[]) => runtimeWatchFrameFixture({
  machines: machines.map(([id, membership]) => runtimeWatchMachineObservationFixture({ membership, machine: runtimeWatchMachineFixture(id, id) })),
  services: [{ identity: TAILSCALE, service_id: "t".repeat(32) as ServiceId, containers: routers, hook_containers: [] }],
});

const BOTH = [["m1", "up"], ["m2", "up"]] as const;

it("calls a claim's key taken only once every Server runs a new router for longer than a refused one restarts", () => {
  expect(claimOutcome(CLAIMED, cluster(BOTH, placed("m1", "r1", SETTLED), placed("m2", "r2", SETTLED)), NOW)).toBe("took");
  expect(claimOutcome(CLAIMED, cluster(BOTH, placed("m1", "r1", SETTLED), placed("m2", "r2", 10_000)), NOW)).toBe("unknown");
});

it("never counts a router that ran before the claim as the claim's", () => {
  expect(claimOutcome(CLAIMED, cluster([["m1", "up"]], placed("m1", "router-old", SETTLED)), NOW)).toBe("unknown");
  expect(claimOutcome(LAPSED, cluster([["m1", "up"]], placed("m1", "router-old", SETTLED)), NOW)).toBe("failed");
});

it("calls a claim failed once every Server answered and one new router stayed stopped", () => {
  expect(claimOutcome(CLAIMED, cluster(BOTH, placed("m1", "r1", SETTLED), placed("m2", "r2", SETTLED, { state: "exited", code: 0 })), NOW)).toBe("failed");
});

it("keeps a claim unknown while a Server may still be pulling, until the work's bound passes", () => {
  const pulling = cluster(BOTH, placed("m1", "r1", SETTLED, { state: "exited", code: 0 }));
  expect(claimOutcome(CLAIMED, pulling, NOW)).toBe("unknown");
  expect(claimOutcome(LAPSED, pulling, NOW)).toBe("failed");
});

it("keeps a claim unknown while a Server is offline or the Cluster can't be read, until the work's bound passes", () => {
  const offline = cluster([["m1", "up"], ["m2", "down"]], placed("m1", "r1", SETTLED), placed("m2", "r2", SETTLED));
  expect(claimOutcome(CLAIMED, offline, NOW)).toBe("unknown");
  expect(claimOutcome(LAPSED, offline, NOW)).toBe("failed");
  expect(claimOutcome(CLAIMED, null, NOW)).toBe("unknown");
  expect(claimOutcome(LAPSED, null, NOW)).toBe("failed");
});

it("waits out a new router still inside the refusal window even past the work's bound", () => {
  expect(claimOutcome(LAPSED, cluster([["m1", "up"]], placed("m1", "r1", 10_000)), NOW)).toBe("unknown");
});
