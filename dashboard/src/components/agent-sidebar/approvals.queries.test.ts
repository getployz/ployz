import { describe, expect, it } from "vitest";
import { MISMATCHED_RESTART, shownTarget } from "./approvals.queries";

const card = {
  review: {
    effects: [],
    operation: { verb: "restart" as const, name: "web in shop/production", preview: { environment: "env-production", service: "web", only: null } },
  },
};
const restart = (environmentId: string, service: string) => ({
  environmentId,
  service,
  environmentName: "production",
  namespace: "production",
  runtime: service,
  copies: [{ id: `${service}-container`, machineId: "m1" }],
  machines: [],
  environmentMachines: [],
});

describe("shownTarget", () => {
  it("approves the restart the card shows", () => {
    expect(shownTarget(card, restart("env-production", "web"))).toEqual({
      environment: { environmentId: "env-production", environmentName: "production", namespace: "production" },
      runtime: "web",
      copies: [{ id: "web-container", machineId: "m1" }],
      machines: [],
      environmentMachines: [],
    });
  });

  it("approves nothing when Cloud names another Service or Environment than the card", () => {
    expect(() => shownTarget(card, restart("env-production", "payments"))).toThrow(MISMATCHED_RESTART);
    expect(() => shownTarget(card, restart("env-staging", "web"))).toThrow(MISMATCHED_RESTART);
  });

  it("approves nothing for a card that doesn't show a restart", () => {
    const stop = { review: { ...card.review, operation: { ...card.review.operation, verb: "stop" as const } } };
    expect(() => shownTarget(stop, restart("env-production", "web"))).toThrow(MISMATCHED_RESTART);
  });
});
