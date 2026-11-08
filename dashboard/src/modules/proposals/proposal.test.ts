import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { MISMATCHED_PROPOSAL, PROPOSAL_CAPABILITY, shownSteps, stepLoss, stepTitle, strandedByProposal } from "./proposal";

const stop = { verb: "stop" as const, service: "web", only: null, namespace: "shop-prod-a1b2", runtime: "web" };
const start = { ...stop, verb: "start" as const };
const copies = [{ id: "c".repeat(64), machineId: "m1" as MachineId, hook: false }];

describe("stepTitle", () => {
  it("names the verb, the Service and, when one is named, the Server", () => {
    expect(stepTitle(stop)).toBe("Stop web");
    expect(stepTitle({ ...stop, only: { machine: "m1" as MachineId, name: "fra-1" } })).toBe("Stop web on fra-1");
  });
});

describe("stepLoss", () => {
  it("names what a stop or restart takes away, as the Approval card does, and nothing for a start", () => {
    expect(stepLoss(stop)).toEqual({ verb: "Stops service", name: "web", after: " until started again" });
    expect(stepLoss({ ...stop, verb: "restart" })).toEqual({ verb: "Restarts service", name: "web", after: "" });
    expect(stepLoss(start)).toBeNull();
  });
});

describe("shownSteps", () => {
  it("signs only the card's steps, in the card's order, with the Containers Cloud names for each", () => {
    const fetched = [{ ...stop, copies }, { ...start, copies: [] }];
    expect(shownSteps([stop, start], fetched)).toEqual([{ verb: "stop", runtime: "web", copies }, { verb: "start", runtime: "web", copies: [] }]);
  });

  it("signs nothing when Cloud adds, drops, reorders or swaps a step", () => {
    expect(() => shownSteps([stop, start], [{ ...stop, copies }])).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps([stop], [{ ...stop, copies }, { ...start, copies }])).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps([stop, start], [{ ...start, copies }, { ...stop, copies }])).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps([stop], [{ ...stop, service: "db", copies }])).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps([stop], [{ ...stop, runtime: "db", copies }])).toThrow(MISMATCHED_PROPOSAL);
  });
});

describe("strandedByProposal", () => {
  const host = { id: "m1", name: "fra-1", capabilities: ["ployz.restart.session-grant.v1"], deviceEnforcement: true };
  it("names an enforcing host that hasn't learnt proposals, and no other", () => {
    expect(strandedByProposal(copies, [host])).toBe("fra-1");
    expect(strandedByProposal(copies, [{ ...host, capabilities: [...host.capabilities, PROPOSAL_CAPABILITY] }])).toBeNull();
    expect(strandedByProposal(copies, [{ ...host, deviceEnforcement: false }])).toBeNull();
    expect(strandedByProposal(copies, [{ ...host, id: "m2" }])).toBeNull();
  });
});
