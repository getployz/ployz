import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { endOf, type ServiceActBefore, type ServiceActLeg } from "./service-act";

const before: ServiceActBefore = { copies: [{ server: "a", machine: "m-a", container: "web-1", state: "running" }], unanswered: ["b"] };
const moved: ServiceActLeg[] = [{ server: "a", machine: "m-a", container: "web-1", stop: "done", start: "done", reason: null }];

describe("endOf", () => {
  it("leaves a Service-wide act unknown when a Server didn't answer, though every copy it saw moved", () => {
    expect(endOf(before, moved, null)).toEqual({ state: "unknown", reason: "b didn't answer." });
  });

  it("ends a Server's own act done when only another Server didn't answer", () => {
    expect(endOf(before, moved, { machine: "m-a" as MachineId, name: "a" })).toEqual({ state: "done", reason: null });
  });
});
