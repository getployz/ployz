import type { ContainerObservation, RuntimeWatchView, ServiceId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";

import { runtimeWatchContainerFixture, runtimeWatchFrameFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";

import { onIngressChange } from "./ingress-running";

const frame = (...states: ContainerObservation["runtime"][]): RuntimeWatchView =>
  runtimeWatchFrameFixture({
    services: [{
      identity: "ployz-system/ingress",
      service_id: "i".repeat(32) as ServiceId,
      containers: states.map((runtime, index) => ({ ...runtimeWatchContainerFixture(`m${index}`, `ingress-${index}`), runtime })),
      hook_containers: [],
    }],
  });

async function changes(frames: RuntimeWatchView[]): Promise<number> {
  let count = 0;
  async function* source() {
    yield* frames;
  }
  for await (const _ of onIngressChange(source(), () => count++)) void _;
  return count;
}

describe("onIngressChange", () => {
  it.each([
    ["paused to exited", { state: "paused" }, { state: "exited", code: 0 }],
    ["restarting to exited", { state: "restarting" }, { state: "exited", code: 0 }],
    ["exited to paused", { state: "exited", code: 0 }, { state: "paused" }],
    ["exited to restarting", { state: "exited", code: 0 }, { state: "restarting" }],
    ["running to exited", { state: "running", health: "healthy" }, { state: "exited", code: 0 }],
  ] as const)("calls once when an Ingress Proxy goes %s", async (_, from, to) => {
    expect(await changes([frame(from), frame(to)])).toBe(1);
  });

  it.each([
    ["paused to restarting", { state: "paused" }, { state: "restarting" }],
    ["exited to dead", { state: "exited", code: 0 }, { state: "dead" }],
  ] as const)("stays quiet when an Ingress Proxy goes %s", async (_, from, to) => {
    expect(await changes([frame(from), frame(to)])).toBe(0);
  });
});
