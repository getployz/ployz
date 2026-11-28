import type { EnvironmentListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { startFromChoices } from "./start-from";

const environment = (name: string, more: Partial<EnvironmentListing> = {}): EnvironmentListing => ({
  id: name, name, default: false, parent: null, removal: null, merged: false, branch_setup: [], protected: false, protections: 0, ...more,
});

describe("startFromChoices", () => {
  it("never offers production or a Protected environment as a preview's copy", () => {
    const choices = startFromChoices([environment("production", { default: true }), environment("live", { protected: true }), environment("staging")]);
    expect(choices.filter((choice) => choice.never === null).map((choice) => choice.name)).toEqual(["staging"]);
  });

  it("never offers a Branch, which the Store refuses as a start-from", () => {
    const choices = startFromChoices([environment("staging"), environment("pr-140", { parent: "staging" })]);
    expect(choices.map((choice) => choice.name)).toEqual(["staging"]);
  });
});
