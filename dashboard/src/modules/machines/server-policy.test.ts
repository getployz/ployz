import { describe, expect, it } from "vitest";
import {
  machineUpdateForPolicyChange,
  policyChangeObserved,
} from "#/modules/machines/server-policy";

describe("Server Policy", () => {
  it("sends only the changed values to the Engine", () => {
    expect(machineUpdateForPolicyChange({ acceptsBuilds: false })).toEqual({
      accepts_builds: false,
    });
    expect(machineUpdateForPolicyChange({ buildConcurrency: "automatic" })).toEqual({
      build_concurrency: { action: "automatic" },
    });
    expect(machineUpdateForPolicyChange({ buildConcurrency: 4 })).toEqual({
      build_concurrency: { action: "set", value: 4 },
    });
  });

  it("settles a requested change only once observation shows all of it", () => {
    const observed = { acceptsBuilds: true, buildConcurrency: null };
    expect(policyChangeObserved(observed, { buildConcurrency: "automatic" })).toBe(true);
    expect(policyChangeObserved(observed, { buildConcurrency: 2 })).toBe(false);
    expect(
      policyChangeObserved(observed, { acceptsBuilds: true, buildConcurrency: 2 }),
    ).toBe(false);
    expect(
      policyChangeObserved(
        { acceptsBuilds: false, buildConcurrency: 2 },
        { acceptsBuilds: false, buildConcurrency: 2 },
      ),
    ).toBe(true);
  });
});
