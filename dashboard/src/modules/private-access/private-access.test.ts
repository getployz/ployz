import type { ContainerObservation, ServiceId } from "@ployz/sdk";
import { expect, it } from "vitest";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { privateAccessObservation, routersOn, TAILSCALE } from "./private-access";

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

it("calls a router stopped until its node joined the tailnet, since Docker keeps restarting one Tailscale refused the key for", () => {
  const routers = routersOn(frame(
    ["m1", { state: "running", health: "healthy" }],
    ["m2", { state: "running", health: "starting" }],
    ["m3", { state: "running", health: "unhealthy" }],
    ["m4", { state: "running", health: "failing" }],
    ["m5", { state: "running", health: "not_configured" }],
  ));
  expect(Object.fromEntries(routers)).toEqual({ m1: "running", m2: "stopped", m3: "stopped", m4: "stopped", m5: "running" });
});

it("calls a Server running if any of its routers runs, whatever order they are listed in", () => {
  expect(routersOn(frame(["m1", { state: "running", health: "none" }], ["m1", { state: "exited", code: 1 }])).get("m1")).toBe("running");
  expect(routersOn(frame(["m1", { state: "exited", code: 1 }], ["m1", { state: "running", health: "none" }])).get("m1")).toBe("running");
});

it("reads Private Access as enforced once any Server enforces Device approval, so Connect asks the passkey", () => {
  const server = (id: string, enforcing: boolean | null) => {
    const machine = runtimeWatchMachineFixture(id, id);
    return runtimeWatchMachineObservationFixture({ machine: { ...machine, runtime: { ...machine.runtime, device_enforcement: enforcing } } });
  };
  expect(privateAccessObservation(runtimeWatchFrameFixture({ machines: [server("m1", false), server("m2", null)] })).enforced).toBe(false);
  expect(privateAccessObservation(runtimeWatchFrameFixture({ machines: [server("m1", false), server("m2", true)] })).enforced).toBe(true);
});
