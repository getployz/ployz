import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { endOf, type EnforcementFrame, enforcesIn, progressOf, type ServiceActBefore, type ServiceActLeg } from "./service-act";

const before: ServiceActBefore = { copies: [{ server: "a", machine: "m-a", container: "web-1", state: "running", hook: false }], unanswered: [{ machine: "m-b" as MachineId, name: "b" }] };
const moved: ServiceActLeg[] = [{ server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: "done", reason: null }];

describe("endOf", () => {
  it("leaves a Service-wide act unknown when a Server didn't answer, though every copy it saw moved", () => {
    expect(endOf("restart", before, moved, null)).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("ends a Server's own act done when only another Server didn't answer", () => {
    expect(endOf("restart", before, moved, { machine: "m-a" as MachineId, name: "a" })).toEqual({ state: "done", reason: null });
  });

  it("ends a Server's own act failed when that Server answered with no containers, whoever else stayed silent", () => {
    expect(endOf("restart", before, [], { machine: "m-a" as MachineId, name: "a" })).toEqual({ state: "failed", reason: "No containers to move." });
  });

  it("leaves a Server's own act with no containers unknown when that Server didn't answer", () => {
    expect(endOf("restart", before, [], { machine: "m-b" as MachineId, name: "b" })).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("matches a Server's own act to its silent Server by Machine, though the Server was renamed after the act was asked", () => {
    expect(endOf("restart", before, [], { machine: "m-b" as MachineId, name: "old-b" })).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("never reads a restart that stopped rolling as done, and numbers the container that didn't come back", () => {
    const answered: ServiceActBefore = { ...before, unanswered: [] };
    const halted: ServiceActLeg[] = [
      { server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: "done", reason: null },
      { server: "a", machine: "m-a", container: "web-2", hook: false, stop: "done", start: "failed", reason: "a couldn't start it." },
      { server: "b", machine: "m-b", container: "web-3", hook: false, stop: null, start: null, reason: null },
    ];
    const reason = "Container 2 on a didn't come back. a couldn't start it. The rest are untouched.";
    expect(endOf("restart", answered, halted, null)).toEqual({ state: "failed", reason });
    expect(endOf("restart", answered, halted.map((leg) => leg.container === "web-2" ? { ...leg, start: "unknown" as const } : leg), null))
      .toEqual({ state: "unknown", reason });
  });

  it("numbers the container that didn't come back when it was the last to roll", () => {
    const answered: ServiceActBefore = { ...before, unanswered: [] };
    const halted: ServiceActLeg[] = [
      { server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: "done", reason: null },
      { server: "a", machine: "m-a", container: "web-2", hook: false, stop: "done", start: "failed", reason: "a couldn't start it." },
    ];
    expect(endOf("restart", answered, halted, null)).toEqual({ state: "failed", reason: "Container 2 on a didn't come back. a couldn't start it." });
  });
});

describe("progressOf", () => {
  const legs: ServiceActLeg[] = [
    { server: "a", machine: "m-a", container: "hook-1", hook: true, stop: "done", start: null, reason: null },
    { server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: "done", reason: null },
    { server: "b", machine: "m-b", container: "web-2", hook: false, stop: "done", start: "sent", reason: null },
    { server: "b", machine: "m-b", container: "web-3", hook: false, stop: null, start: null, reason: null },
  ];
  const act = { id: "act-1", state: "running" as const, legs, reason: null, only: null };

  it("counts a restart by the containers it starts again, hooks aside", () => {
    expect(progressOf({ ...act, verb: "restart" })).toMatchObject({ total: 3, done: 1 });
  });

  it("counts a stop by every container it stops, hooks included", () => {
    expect(progressOf({ ...act, verb: "stop" })).toMatchObject({ total: 4, done: 3 });
  });

  it("gives a failed stop its own reason, not a restart's", () => {
    const answered: ServiceActBefore = { ...before, unanswered: [] };
    const stopped: ServiceActLeg[] = [
      { server: "a", machine: "m-a", container: "web-1", hook: false, stop: "done", start: null, reason: null },
      { server: "a", machine: "m-a", container: "web-2", hook: false, stop: "failed", start: null, reason: "a couldn't stop it." },
    ];
    expect(endOf("stop", answered, stopped, null)).toEqual({ state: "failed", reason: "a couldn't stop it." });
  });
});

describe("enforcesIn", () => {
  const copy = (machine: string) => ({ container_id: `c-${machine}`, machine_id: machine });
  const frame = (enforcement: boolean | null | undefined): EnforcementFrame => ({
    services: [
      { identity: "shop/web", containers: [copy("m-a")], hook_containers: [] },
      { identity: "shop/api", containers: [copy("m-b")], hook_containers: [copy("m-c")] },
    ],
    machines: [
      { machine: { id: "m-a", runtime: enforcement === undefined ? {} : { device_enforcement: enforcement } } },
      { machine: { id: "m-b", runtime: { device_enforcement: false } } },
      { machine: { id: "m-c", runtime: { device_enforcement: true } } },
    ],
  });

  it("asks for an approval when a Server running the Service's copies enforces", () => {
    expect(enforcesIn(frame(true), "shop/web")).toBe(true);
  });

  it("asks nothing when its Servers don't enforce or never said, though another Service's Server in the Environment does", () => {
    expect(enforcesIn(frame(false), "shop/web")).toBe(false);
    expect(enforcesIn(frame(null), "shop/web")).toBe(false);
    expect(enforcesIn(frame(undefined), "shop/web")).toBe(false);
  });

  it("counts a Server that runs only the Service's hook", () => {
    expect(enforcesIn(frame(false), "shop/api")).toBe(true);
  });

  it("asks nothing for a Service no Server reports", () => {
    expect(enforcesIn(frame(true), "shop/worker")).toBe(false);
  });
});
