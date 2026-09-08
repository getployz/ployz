import { describe, expect, it } from "vitest";
import { getDeployTargetPreflight } from "#/modules/runtime/deploy-target-preflight";

describe("deploy target preflight", () => {
  it("allows deploy when runtime has machines", () => {
    expect(
      getDeployTargetPreflight({
        status: "observed",
        machineCount: 1,
        isLoading: false,
        error: null,
      }),
    ).toEqual({ ok: true });
  });

  it("blocks deploy when pairing is present but the cluster is unreachable", () => {
    expect(
      getDeployTargetPreflight({
        status: "unreachable",
        machineCount: 0,
        isLoading: false,
        error: "The cluster is expected but unreachable.",
      }),
    ).toMatchObject({
      ok: false,
      title: "Runtime unavailable",
      action: null,
    });
  });
});
