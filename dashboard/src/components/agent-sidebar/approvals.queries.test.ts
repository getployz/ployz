import type { DestructiveEffect, MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { MISMATCHED_ACT, shownTarget } from "./approvals.queries";

const effect = (verb: "restart" | "stop", name: string): DestructiveEffect =>
  ({ kind: verb === "stop" ? "stops_service" : "restarts_service", name, node: `env-production/${name}`, path: `services.${name}` });

const preview = { environment: "env-production", service: "web", namespace: "production", runtime: "web", only: null };
const card = (verb: "restart" | "stop" = "restart", shown: Record<string, string | null | { machine: string }> = preview, name = "web in shop/production") => ({
  command: verb,
  review: { effects: [effect(verb, "web")], operation: { verb, name, preview: shown } },
});
const target = (changes: Partial<{ verb: "restart" | "stop"; environmentId: string; service: string; namespace: string; runtime: string }> = {}) => ({
  verb: "restart" as const,
  environmentId: "env-production",
  service: "web",
  project: "shop",
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

  it("approves nothing when the card's title or Destroys line names another Service or place than what it signs", () => {
    // A row edited so the card reads "web" while its preview, the signed part, names "api".
    expect(() => shownTarget(card("restart", { ...preview, service: "api", runtime: "api" }, "web in shop/production"), target({ service: "api", runtime: "api" }))).toThrow(MISMATCHED_ACT);
    expect(() => shownTarget(card("restart", preview, "web in blog/production"), target())).toThrow(MISMATCHED_ACT);
    expect(() => shownTarget(card("restart", preview, "web in shop/staging"), target())).toThrow(MISMATCHED_ACT);
    const stop = card("stop");
    stop.review.effects = [effect("stop", "api")];
    expect(() => shownTarget(stop, target({ verb: "stop" }))).toThrow(MISMATCHED_ACT);
  });

  it("approves a stop whose title names the Server the ask is limited to, and refuses one naming another", () => {
    const only = { ...preview, only: { machine: "m1" } };
    const machines = [{ id: "m1" as MachineId, name: "fra-1", capabilities: [], sealing: null, deviceEnforcement: false }];
    expect(shownTarget(card("stop", only, "web in shop/production on fra-1"), { ...target({ verb: "stop" }), machines }).verb).toBe("stop");
    expect(() => shownTarget(card("stop", only, "web in shop/production on fra-2"), { ...target({ verb: "stop" }), machines })).toThrow(MISMATCHED_ACT);
    expect(() => shownTarget(card("stop", only, "web in shop/production"), { ...target({ verb: "stop" }), machines })).toThrow(MISMATCHED_ACT);
  });

  it("approves nothing for an ask that doesn't name its runtime", () => {
    expect(() => shownTarget(card("restart", { environment: "env-production", service: "web", only: null }), target())).toThrow(MISMATCHED_ACT);
  });
});
