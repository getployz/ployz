import { describe, expect, it } from "vitest";
import { Result } from "effect";
import {
  parseDeployOperationEvidence,
  parseTypedDeployFailureEvidence,
} from "#/modules/operations/deploy-operation-evidence";

const operationId = "deploy-operation-1";

describe("deploy operation evidence", () => {
  it("accepts approved storage evidence and rejects injected details", () => {
    expect(
      Result.isSuccess(
        parseDeployOperationEvidence({
        eventType: "deploy_failed",
        payload: {
          operationId,
          failure: {
            kind: "no_usable_machines",
            reasons: [
              {
                machineId: "machine-1",
                reason: {
                  kind: "storage_unavailable",
                  reason: { reason: "pool_faulted", pool: "tank" },
                },
              },
            ],
          },
        },
        }),
      ),
    ).toBe(true);
    expect(
      Result.isSuccess(
        parseDeployOperationEvidence({
        eventType: "deploy_failed",
        payload: {
          operationId,
          failure: {
            kind: "no_usable_machines",
            reasons: [
              {
                machineId: "machine-1",
                reason: {
                  kind: "storage_unavailable",
                  reason: {
                    reason: "pool_faulted",
                    pool: "tank",
                    message: "private pool detail",
                  },
                },
              },
            ],
          },
        },
        }),
      ),
    ).toBe(false);
  });

  it("accepts kind-only historical evidence but requires typed detail", () => {
    expect(
      Result.isSuccess(
        parseDeployOperationEvidence({
        eventType: "deploy_failed",
        payload: {
          operationId,
          failure: { kind: "automatic_hostname_collision" },
        },
        }),
      ),
    ).toBe(true);
    expect(
      Result.isSuccess(
        parseTypedDeployFailureEvidence({
        kind: "automatic_hostname_collision",
        }),
      ),
    ).toBe(false);
    expect(
      Result.isSuccess(
        parseTypedDeployFailureEvidence({
        kind: "automatic_hostname_collision",
        hostname: "api.production.ployz.app",
        routeBindingId: "route-binding-1",
        }),
      ),
    ).toBe(true);
  });
});
