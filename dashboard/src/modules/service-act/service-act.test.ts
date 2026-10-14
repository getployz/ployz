import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { endOf, progressOf, type ServiceActBefore, type ServiceActLeg } from "./service-act";

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
