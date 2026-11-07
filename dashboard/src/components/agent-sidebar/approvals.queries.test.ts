import { describe, expect, it } from "vitest";
import { MISMATCHED_RESTART, shownTarget } from "./approvals.queries";

const preview = { environment: "env-production", service: "web", namespace: "production", runtime: "web", only: null };
const card = (shown: Record<string, string | null> = preview) => ({
  review: { effects: [], operation: { verb: "restart" as const, name: "web in shop/production", preview: shown } },
});
const target = (changes: Partial<{ environmentId: string; service: string; namespace: string; runtime: string }> = {}) => ({
  environmentId: "env-production",
  service: "web",
  environmentName: "production",
  namespace: "production",
  runtime: "web",
  copies: [{ id: `${changes.runtime ?? "web"}-container`, machineId: "m1" }],
  machines: [],
  environmentMachines: [],
  ...changes,
});

describe("shownTarget", () => {
  it("approves the restart the card shows", () => {
    expect(shownTarget(card(), target())).toEqual({
      environment: { environmentId: "env-production", environmentName: "production", namespace: "production" },
      runtime: "web",
      copies: [{ id: "web-container", machineId: "m1" }],
      machines: [],
      environmentMachines: [],
    });
  });

  it("approves nothing when Cloud names another Service or Environment than the card", () => {
    expect(() => shownTarget(card(), target({ service: "payments" }))).toThrow(MISMATCHED_RESTART);
    expect(() => shownTarget(card(), target({ environmentId: "env-staging" }))).toThrow(MISMATCHED_RESTART);
  });

  it("approves nothing when Cloud keeps the card's Service but names another runtime or Namespace", () => {
    expect(() => shownTarget(card(), target({ runtime: "payments" }))).toThrow(MISMATCHED_RESTART);
    expect(() => shownTarget(card(), target({ namespace: "billing" }))).toThrow(MISMATCHED_RESTART);
  });

  it("approves nothing for a card that doesn't show a restart, or an ask that doesn't name its runtime", () => {
    const stop = { review: { ...card().review, operation: { ...card().review.operation, verb: "stop" as const } } };
    expect(() => shownTarget(stop, target())).toThrow(MISMATCHED_RESTART);
    expect(() => shownTarget(card({ environment: "env-production", service: "web", only: null }), target())).toThrow(MISMATCHED_RESTART);
  });
});
