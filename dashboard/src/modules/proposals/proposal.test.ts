import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { stepLoss, stepTitle } from "./proposal";

const stop = { verb: "stop" as const, service: "web", only: null, namespace: "shop-prod-a1b2", runtime: "web" };
const start = { ...stop, verb: "start" as const };
const renamed = { ...stop, service: "web", runtime: "web-old" };

describe("stepTitle", () => {
  it("names the Server only when the step is held to one", () => {
    expect(stepTitle({ ...stop, only: { machine: "m1" as MachineId, name: "fra-1" } })).toContain("fra-1");
    expect(stepTitle(stop)).not.toContain("fra-1");
  });

  it("names a renamed Service both as the servers run it and as it is named now", () => {
    expect(stepTitle(renamed)).toContain("web-old");
    expect(stepTitle(renamed)).toContain("(web)");
  });
});

describe("stepLoss", () => {
  it("names what a stop or restart takes away, and nothing for a start", () => {
    expect(stepLoss(stop)).toMatchObject({ name: "web" });
    expect(stepLoss({ ...stop, verb: "restart" })).toMatchObject({ name: "web" });
    expect(stepLoss(renamed)?.name).toContain("web-old");
    expect(stepLoss(start)).toBeNull();
  });
});
