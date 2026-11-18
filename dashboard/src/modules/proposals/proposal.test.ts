import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { stepLoss, stepTitle } from "./proposal";

const stop = { verb: "stop" as const, service: "web", only: null, namespace: "shop-prod-a1b2", runtime: "web" };
const start = { ...stop, verb: "start" as const };
const renamed = { ...stop, service: "web", runtime: "web-old" };

describe("stepTitle", () => {
  it("names the verb, the Service and, when one is named, the Server", () => {
    expect(stepTitle(stop)).toBe("Stop web");
    expect(stepTitle({ ...stop, only: { machine: "m1" as MachineId, name: "fra-1" } })).toBe("Stop web on fra-1");
  });

  it("names a renamed Service as the servers run it, with the name it has now after", () => {
    expect(stepTitle(renamed)).toBe("Stop web-old (web)");
  });
});

describe("stepLoss", () => {
  it("names what a stop or restart takes away, as the Approval card does, and nothing for a start", () => {
    expect(stepLoss(stop)).toEqual({ verb: "Stops service", name: "web", after: " until started again" });
    expect(stepLoss({ ...stop, verb: "restart" })).toEqual({ verb: "Restarts service", name: "web", after: "" });
    expect(stepLoss(renamed)).toEqual({ verb: "Stops service", name: "web-old (web)", after: " until started again" });
    expect(stepLoss(start)).toBeNull();
  });
});
