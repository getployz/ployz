import type {
  PhaseAwareDeployServiceResult,
  ValidatedPhaseAwareDeployResult,
} from "#/modules/runtime/phase-aware-deploy-contract";

export type PhaseAwareAppliedStateBinding<TNode> = {
  serviceId: string;
  node: TNode;
};

function advancesAppliedState(result: PhaseAwareDeployServiceResult) {
  return (
    result.result === "applied" ||
    result.result === "removed" ||
    result.result === "unchanged"
  );
}

/**
 * Fold confirmed per-Service runtime evidence into Applied State. Ambiguous or
 * negative evidence retains the prior binding (including prior absence), while
 * confirmed evidence advances to or satisfies the complete Attempt Target.
 */
export function foldPhaseAwareAppliedState<TNode>(input: {
  prior: readonly PhaseAwareAppliedStateBinding<TNode>[];
  target: readonly PhaseAwareAppliedStateBinding<TNode>[];
  result: ValidatedPhaseAwareDeployResult;
}): PhaseAwareAppliedStateBinding<TNode>[] {
  const applied = new Map(
    input.prior.map((binding) => [binding.serviceId, binding] as const),
  );
  const target = new Map(
    input.target.map((binding) => [binding.serviceId, binding] as const),
  );

  for (const phase of input.result.phases) {
    for (const service of phase.services) {
      if (!advancesAppliedState(service)) continue;

      const targetBinding = target.get(service.service_id);
      if (service.result === "applied") {
        if (!targetBinding) {
          throw new Error(
            `Applied Service ${service.service_id} is missing from the Attempt Target.`,
          );
        }
        applied.set(service.service_id, targetBinding);
        continue;
      }

      if (service.result === "unchanged" && targetBinding) {
        applied.set(service.service_id, targetBinding);
      } else {
        applied.delete(service.service_id);
      }
    }
  }

  return [...applied.values()];
}
