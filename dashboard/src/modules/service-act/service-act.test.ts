import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { endOf, type ServiceActBefore, type ServiceActLeg } from "./service-act";

const before: ServiceActBefore = { copies: [{ server: "a", machine: "m-a", container: "web-1", state: "running", hook: false }], unanswered: [{ machine: "m-b" as MachineId, name: "b" }] };
const moved: ServiceActLeg[] = [{ server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: "done", reason: null }];

describe("endOf", () => {
  it("leaves a Service-wide act unknown when a Server didn't answer, though every copy it saw moved", () => {
    expect(endOf(before, moved, null)).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("ends a Server's own act done when only another Server didn't answer", () => {
    expect(endOf(before, moved, { machine: "m-a" as MachineId, name: "a" })).toEqual({ state: "done", reason: null });
  });

  it("ends a Server's own act failed when that Server answered with no containers, whoever else stayed silent", () => {
    expect(endOf(before, [], { machine: "m-a" as MachineId, name: "a" })).toEqual({ state: "failed", reason: "No containers to move." });
  });

  it("leaves a Server's own act with no containers unknown when that Server didn't answer", () => {
    expect(endOf(before, [], { machine: "m-b" as MachineId, name: "b" })).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("matches a Server's own act to its silent Server by Machine, though the Server was renamed after the act was asked", () => {
    expect(endOf(before, [], { machine: "m-b" as MachineId, name: "old-b" })).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("never reads a restart that stopped rolling as done, and names the Server that didn't come back", () => {
    const answered: ServiceActBefore = { ...before, unanswered: [] };
    const halted: ServiceActLeg[] = [
      { server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: "failed", reason: "a couldn't start it." },
      { server: "b", machine: "m-b", container: "web-2", hook: false, stop: null, start: null, reason: null },
    ];
    expect(endOf(answered, halted, null)).toEqual({ state: "failed", reason: "a didn't come back, so Cloud left b alone." });
    expect(endOf(answered, halted.map((leg) => leg.server === "a" ? { ...leg, start: "unknown" as const } : leg), null))
      .toEqual({ state: "unknown", reason: "a didn't come back, so Cloud left b alone." });
  });
});
