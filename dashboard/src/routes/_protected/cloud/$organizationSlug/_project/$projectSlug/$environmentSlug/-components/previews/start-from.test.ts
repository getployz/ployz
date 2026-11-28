import type { EnvironmentListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { startFromChoices } from "./start-from";

const environment = (name: string, more: Partial<EnvironmentListing> = {}): EnvironmentListing => ({
  id: name, name, default: false, parent: null, removal: null, merged: false, branch_setup: [], protected: false, protections: 0, ...more,
});

describe("startFromChoices", () => {
  it("never offers production or a Protected environment as a preview's copy", () => {
    const choices = startFromChoices([environment("production", { default: true }), environment("live", { protected: true }), environment("staging")], new Set());
    expect(choices.filter((choice) => choice.never === null).map((choice) => choice.name)).toEqual(["staging"]);
  });
});
