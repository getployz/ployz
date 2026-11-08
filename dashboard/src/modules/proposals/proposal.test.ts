import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { MISMATCHED_PROPOSAL, PROPOSAL_CAPABILITY, type ProposalStep, shownSteps, stepLoss, stepTitle, strandedByProposal } from "./proposal";

const stop = { verb: "stop" as const, service: "web", only: null, namespace: "shop-prod-a1b2", runtime: "web" };
const start = { ...stop, verb: "start" as const };
const renamed = { ...stop, service: "web", runtime: "web-old" };
const copies = [{ id: "c".repeat(64), machineId: "m1" as MachineId, hook: false }];
const card = (...steps: ProposalStep[]) => ({ project: "shop", environment: "production", steps });
const target = (...steps: { verb: "stop" | "start" | "restart"; service: string; runtime: string | null; copies: typeof copies }[]) => ({ project: "shop", environmentName: "production", namespace: "shop-prod-a1b2", steps });

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

describe("shownSteps", () => {
  it("signs only the card's namespace and steps, in the card's order, with the Containers Cloud names for each", () => {
    expect(shownSteps(card(stop, start), target({ ...stop, copies }, { ...start, copies: [] }))).toEqual({
      namespace: "shop-prod-a1b2",
      steps: [{ verb: "stop", runtime: "web", copies }, { verb: "start", runtime: "web", copies: [] }],
    });
  });

  it("signs a renamed Service by the name the servers run, when the card shows that name", () => {
    expect(shownSteps(card(renamed), target({ ...renamed, copies })).steps).toEqual([{ verb: "stop", runtime: "web-old", copies }]);
  });

  it("signs nothing when Cloud adds, drops or swaps a step, or names another namespace", () => {
    expect(() => shownSteps(card(stop, start), target({ ...stop, copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop), target({ ...stop, copies }, { ...start, copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop, start), target({ ...start, copies }, { ...stop, copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop), target({ ...stop, service: "db", copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop), { ...target({ ...stop, copies }), namespace: "shop-prod-ffff" })).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop, { ...start, namespace: "shop-prod-ffff" }), target({ ...stop, copies }, { ...start, copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(), target())).toThrow(MISMATCHED_PROPOSAL);
  });

  it("signs nothing when the card's runtime name isn't the one the Store gives the Service it names", () => {
    // A row edited to carry another Service's runtime name: the card reads `web`, the servers would get `api`.
    expect(() => shownSteps(card({ ...stop, runtime: "api" }), target({ ...stop, copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop), target({ ...stop, runtime: "api", copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps(card(stop), target({ ...stop, runtime: null, copies }))).toThrow(MISMATCHED_PROPOSAL);
  });

  it("signs nothing when the card's place isn't the Environment the proposal belongs to", () => {
    expect(() => shownSteps({ ...card(stop), project: "blog" }, target({ ...stop, copies }))).toThrow(MISMATCHED_PROPOSAL);
    expect(() => shownSteps({ ...card(stop), environment: "staging" }, target({ ...stop, copies }))).toThrow(MISMATCHED_PROPOSAL);
  });

  it("signs a step on one Server only for Containers on that Server", () => {
    const only = { ...stop, only: { machine: "m1" as MachineId, name: "fra-1" } };
    expect(shownSteps(card(only), target({ ...stop, copies })).steps[0]?.copies).toEqual(copies);
    expect(() => shownSteps(card(only), target({ ...stop, copies: [...copies, { id: "d".repeat(64), machineId: "m2" as MachineId, hook: false }] }))).toThrow(MISMATCHED_PROPOSAL);
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
