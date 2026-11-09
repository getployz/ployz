import type { ContainerObservation, ServiceId } from "@ployz/sdk";
import { expect, it } from "vitest";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { routersOn, TAILSCALE } from "./private-access";

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
