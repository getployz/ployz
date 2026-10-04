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
  });

  it("settles a requested change only once observation shows all of it", () => {
    const observed = { acceptsBuilds: true, acceptsServices: true, buildConcurrency: null };
    expect(policyChangeObserved(observed, { buildConcurrency: "automatic" })).toBe(true);
    expect(policyChangeObserved(observed, { buildConcurrency: 2 })).toBe(false);
    expect(
      policyChangeObserved(observed, { acceptsBuilds: true, buildConcurrency: 2 }),
    ).toBe(false);
    expect(policyChangeObserved(observed, { acceptsServices: true })).toBe(true);
    expect(policyChangeObserved(observed, { acceptsServices: false })).toBe(false);
    expect(
      policyChangeObserved(
        { acceptsBuilds: false, acceptsServices: false, buildConcurrency: 2 },
        { acceptsBuilds: false, acceptsServices: false, buildConcurrency: 2 },
      ),
    ).toBe(true);
  });
});
