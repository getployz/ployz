import { describe, expect, it } from "vitest";
import { toPhaseAwareCurrentRuntimeServiceResult } from "#/modules/runtime/phase-aware-deploy-current-runtime-adapter";

describe("current runtime phase-aware deploy adapter", () => {
  it("isolates current phase evidence translation from Applied folding", () => {
    expect(
      [
        { result: "completed" as const, serviceId: "api" },
        { result: "removed" as const, serviceId: "worker" },
        { result: "unchanged" as const, serviceId: "cache" },
        {
          result: "failed" as const,
          serviceId: "database",
          failure: { kind: "healthcheck_failed" },
        },
        { result: "skipped" as const, serviceId: "web" },
      ].map(toPhaseAwareCurrentRuntimeServiceResult),
    ).toEqual([
      { service_id: "api", result: "applied" },
      { service_id: "worker", result: "removed" },
      { service_id: "cache", result: "unchanged" },
      {
        service_id: "database",
        result: "failed",
        failure: {
          code: "healthcheck_failed",
          message: "Current Runtime reported healthcheck_failed.",
        },
      },
      {
        service_id: "web",
        result: "skipped",
        reason: {
          code: "current_runtime_skipped",
          message: "Current Runtime skipped this Service.",
        },
      },
    ]);
  });
});
