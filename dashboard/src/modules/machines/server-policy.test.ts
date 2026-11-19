import { describe, expect, it } from "vitest";
import {
  isEmptyPolicyChange,
  machineUpdateForPolicyChange,
  policyChangeObserved,
  ServerPolicyChangeSchema,
} from "#/modules/machines/server-policy";
import { Schema } from "effect";

describe("Server Policy", () => {
  it("sends only the changed values to the Engine", () => {
    expect(machineUpdateForPolicyChange({ acceptsBuilds: false })).toEqual({
      accepts_builds: false,
    });
    expect(machineUpdateForPolicyChange({ acceptsServices: false })).toEqual({
      accepts_services: false,
    });
    expect(machineUpdateForPolicyChange({ acceptsIngress: false })).toEqual({
      accepts_ingress: false,
    });
    expect(machineUpdateForPolicyChange({ buildConcurrency: "automatic" })).toEqual({
      build_concurrency: { action: "automatic" },
    });
    expect(machineUpdateForPolicyChange({ buildConcurrency: 4 })).toEqual({
      build_concurrency: { action: "set", value: 4 },
    });
  });

  it("accepts the services role in a change, and calls a change that sets nothing empty", () => {
    expect(Schema.decodeUnknownSync(ServerPolicyChangeSchema)({ acceptsServices: true })).toEqual({ acceptsServices: true });
    expect(isEmptyPolicyChange({})).toBe(true);
    expect(isEmptyPolicyChange({ acceptsServices: false })).toBe(false);
    expect(isEmptyPolicyChange({ acceptsBuilds: true })).toBe(false);
    expect(isEmptyPolicyChange({ acceptsIngress: false })).toBe(false);
    expect(Schema.decodeUnknownOption(ServerPolicyChangeSchema)({ acceptsIngress: true })._tag).toBe("None");
  });

  it("settles a requested change only once observation shows all of it", () => {
    const observed = { acceptsBuilds: true, acceptsServices: true, acceptsIngress: true, buildConcurrency: null, region: null };
    expect(policyChangeObserved(observed, { buildConcurrency: "automatic" })).toBe(true);
    expect(policyChangeObserved(observed, { buildConcurrency: 2 })).toBe(false);
    expect(
      policyChangeObserved(observed, { acceptsBuilds: true, buildConcurrency: 2 }),
    ).toBe(false);
    expect(policyChangeObserved(observed, { acceptsServices: true })).toBe(true);
    expect(policyChangeObserved(observed, { acceptsServices: false })).toBe(false);
    expect(policyChangeObserved(observed, { acceptsIngress: false })).toBe(false);
    expect(policyChangeObserved({ ...observed, acceptsIngress: false }, { acceptsIngress: false })).toBe(true);
    expect(
      policyChangeObserved(
        { acceptsBuilds: false, acceptsServices: false, acceptsIngress: true, buildConcurrency: 2, region: null },
        { acceptsBuilds: false, acceptsServices: false, buildConcurrency: 2 },
      ),
    ).toBe(true);
  });

  it("sends a region as a label change and clears it with a null", () => {
    expect(machineUpdateForPolicyChange({ region: "fsn" })).toEqual({ label_changes: { region: "fsn" } });
    expect(machineUpdateForPolicyChange({ region: null })).toEqual({ label_changes: { region: null } });
    expect(isEmptyPolicyChange({ region: null })).toBe(false);
  });

  it("settles a region change only once observation carries the label, or lacks it for a clear", () => {
    const observed = { acceptsBuilds: true, acceptsServices: true, acceptsIngress: true, buildConcurrency: null };
    expect(policyChangeObserved({ ...observed, region: null }, { region: "fsn" })).toBe(false);
    expect(policyChangeObserved({ ...observed, region: "syd" }, { region: "fsn" })).toBe(false);
    expect(policyChangeObserved({ ...observed, region: "fsn" }, { region: "fsn" })).toBe(true);
    expect(policyChangeObserved({ ...observed, region: "fsn" }, { region: null })).toBe(false);
    expect(policyChangeObserved({ ...observed, region: null }, { region: null })).toBe(true);
  });

  it("takes the Engine's region grammar: letters, digits, hyphens, underscores and dots up to 63, in any case", () => {
    const decode = (region: string) => Schema.decodeUnknownOption(ServerPolicyChangeSchema)({ region })._tag;
    for (const good of ["fsn", "FSN", "eu-west-2", "eu_west.1", "a".repeat(63)]) expect(decode(good)).toBe("Some");
    for (const bad of ["new york", "eu/west", "", "a".repeat(64)]) expect(decode(bad)).toBe("None");
  });
});
