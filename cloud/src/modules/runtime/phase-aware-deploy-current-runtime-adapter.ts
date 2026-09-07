import type { PhaseAwareDeployServiceResult } from "#/modules/runtime/phase-aware-deploy-contract";
import type { RuntimePhaseService } from "#/modules/operations/deploy-operation-evidence";

export function toPhaseAwareCurrentRuntimeServiceResult(
  service: RuntimePhaseService,
): PhaseAwareDeployServiceResult {
  switch (service.result) {
    case "completed":
      return { service_id: service.serviceId, result: "applied" };
    case "removed":
    case "unchanged":
      return { service_id: service.serviceId, result: service.result };
    case "failed":
      return {
        service_id: service.serviceId,
        result: service.result,
        failure: {
          code: service.failure.kind,
          message: `Current Runtime reported ${service.failure.kind}.`,
        },
      };
    case "skipped":
      return {
        service_id: service.serviceId,
        result: service.result,
        reason: {
          code: "current_runtime_skipped",
          message: "Current Runtime skipped this Service.",
        },
      };
  }
}
