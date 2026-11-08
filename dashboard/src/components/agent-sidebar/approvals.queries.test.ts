import { describe, expect, it } from "vitest";
import { MISMATCHED_ACT, shownTarget } from "./approvals.queries";

const preview = { environment: "env-production", service: "web", namespace: "production", runtime: "web", only: null };
const card = (verb: "restart" | "stop" = "restart", shown: Record<string, string | null> = preview) => ({
  review: { effects: [], operation: { verb, name: "web in shop/production", preview: shown } },
});
const target = (changes: Partial<{ verb: "restart" | "stop"; environmentId: string; service: string; namespace: string; runtime: string }> = {}) => ({
  verb: "restart" as const,
  environmentId: "env-production",
  service: "web",
  environmentName: "production",
  namespace: "production",
  runtime: "web",
  copies: [{ id: `${changes.runtime ?? "web"}-container`, machineId: "m1", hook: false }],
  machines: [],
  environmentMachines: [],
  ...changes,
});

describe("shownTarget", () => {
  it("approves the restart or stop the card shows", () => {
    for (const verb of ["restart", "stop"] as const) {
      expect(shownTarget(card(verb), target({ verb }))).toEqual({
        verb,
        target: {
          environment: { environmentId: "env-production", environmentName: "production", namespace: "production" },
          runtime: "web",
          copies: [{ id: "web-container", machineId: "m1", hook: false }],
          machines: [],
          environmentMachines: [],
        },
      });
    }
  });

  it("approves nothing when Cloud names another act than the card", () => {
    expect(() => shownTarget(card("stop"), target({ verb: "restart" }))).toThrow(MISMATCHED_ACT);
    expect(() => shownTarget(card("restart"), target({ verb: "stop" }))).toThrow(MISMATCHED_ACT);
  });

  it("approves nothing when Cloud names another Service or Environment than the card", () => {
    expect(() => shownTarget(card(), target({ service: "payments" }))).toThrow(MISMATCHED_ACT);
    expect(() => shownTarget(card(), target({ environmentId: "env-staging" }))).toThrow(MISMATCHED_ACT);
  });

  it("approves nothing when Cloud keeps the card's Service but names another runtime or Namespace", () => {
    expect(() => shownTarget(card(), target({ runtime: "payments" }))).toThrow(MISMATCHED_ACT);
    expect(() => shownTarget(card(), target({ namespace: "billing" }))).toThrow(MISMATCHED_ACT);
  });

  it("approves nothing for an ask that doesn't name its runtime", () => {
    expect(() => shownTarget(card("restart", { environment: "env-production", service: "web", only: null }), target())).toThrow(MISMATCHED_ACT);
  });
});
