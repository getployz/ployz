import { Data, Result, Schema } from "effect";
import {
  githubChangedPathsSchema,
  githubExactShaSchema,
  githubServiceCandidateSchema,
  type GithubBranchCursor,
  type GithubBranchHeadObservation,
  type GithubCompareObservation,
  type GithubEnvironmentTriggerInput,
  type GithubEnvironmentTriggerSelection,
  type GithubServiceCandidate,
} from "#/modules/github/github-ingestion.contracts";
import { matchGithubWatchPaths } from "#/modules/github/github-webhook-contracts";

export type GithubBranchComparison =
  | { state: "not_required" }
  | { state: "not_found" }
  | { state: "observed"; value: GithubCompareObservation };

type ActiveBranch = {
  state: "active";
  headSha: string;
  evaluationReason:
    | "first_observation"
    | "changed_paths"
    | "rebaseline_all_services";
  evaluationRevision: number;
};

type ActivePlan = {
  kind: "active";
  branch: ActiveBranch;
  triggers: GithubEnvironmentTriggerInput[];
  changedPaths: string[];
  selection: GithubEnvironmentTriggerSelection;
  outcome:
    | "branch_projected"
    | "branch_rebased_all_services"
    | "ignored_no_matching_service";
};

type GithubEnvironmentTriggerSelectionReason =
  GithubEnvironmentTriggerSelection["reason"];

const activePlanSemantics = {
  first_observation: {
    evaluationReason: "first_observation",
    outcome: "branch_projected",
  },
  changed_paths: {
    evaluationReason: "changed_paths",
    outcome: "branch_projected",
  },
  force_rebaseline: {
    evaluationReason: "rebaseline_all_services",
    outcome: "branch_rebased_all_services",
  },
  non_ancestor_rebaseline: {
    evaluationReason: "rebaseline_all_services",
    outcome: "branch_rebased_all_services",
  },
  changed_paths_incomplete_rebaseline: {
    evaluationReason: "rebaseline_all_services",
    outcome: "branch_rebased_all_services",
  },
} as const satisfies Record<
  GithubEnvironmentTriggerSelectionReason,
  {
    evaluationReason: ActiveBranch["evaluationReason"];
    outcome: Exclude<
      ActivePlan["outcome"],
      "ignored_no_matching_service"
    >;
  }
>;

export type BranchEvaluationPlan =
  | { kind: "stale"; outcome: "ignored_stale" }
  | {
      kind: "deleted";
      branch: {
        state: "deleted";
        evaluationReason: "branch_deleted";
        evaluationRevision: number;
      };
      outcome: "branch_deleted";
      triggers: [];
    }
  | ActivePlan;

export class GithubBranchEvaluationPolicyError extends Data.TaggedError(
  "GithubBranchEvaluationPolicyError",
)<{
  code: "invalid_input" | "comparison_required" | "invalid_watch_pattern";
  retriable: false;
  message: string;
}> {}

function policyError(
  code: GithubBranchEvaluationPolicyError["code"],
): GithubBranchEvaluationPolicyError {
  return new GithubBranchEvaluationPolicyError({
    code,
    retriable: false,
    message: `GitHub branch evaluation failed (${code}).`,
  });
}

function nextEvaluationRevision(cursor: GithubBranchCursor | null) {
  const revision = (cursor?.evaluationRevision ?? 0) + 1;
  return Number.isSafeInteger(revision) && revision > 0 ? revision : null;
}

export function selectGithubEnvironmentTriggers(input: {
  candidates: readonly GithubServiceCandidate[];
  selection: GithubEnvironmentTriggerSelection;
  changedPaths: string[];
}): Result.Result<
  GithubEnvironmentTriggerInput[],
  GithubBranchEvaluationPolicyError
> {
  const servicesByEnvironment = new Map<string, Set<string>>();
  for (const candidate of input.candidates) {
    if (input.selection.mode === "paths") {
      const selected = matchGithubWatchPaths({
        changedPaths: input.changedPaths,
        watchPaths: candidate.watchPaths,
      });
      if (Result.isFailure(selected)) {
        return Result.fail(policyError("invalid_watch_pattern"));
      }
      if (!selected.success) continue;
    }
    const services = servicesByEnvironment.get(candidate.environmentId);
    if (services) services.add(candidate.serviceId);
    else {
      servicesByEnvironment.set(
        candidate.environmentId,
        new Set([candidate.serviceId]),
      );
    }
  }
  return Result.succeed(
    Array.from(servicesByEnvironment, ([environmentId, serviceIds]) => ({
      environmentId,
      serviceIds: Array.from(serviceIds).sort((left, right) =>
        left.localeCompare(right),
      ),
      selection: input.selection,
    })).sort((left, right) =>
      left.environmentId.localeCompare(right.environmentId),
    ),
  );
}

export function githubBranchEvaluationOutcome(
  selection: GithubEnvironmentTriggerSelection,
  triggerCount: number,
): ActivePlan["outcome"] {
  return triggerCount > 0
    ? activePlanSemantics[selection.reason].outcome
    : "ignored_no_matching_service";
}

export function planGithubBranchEvaluation(input: {
  cursor: GithubBranchCursor | null;
  liveBranch: GithubBranchHeadObservation;
  forced: boolean;
  comparison: GithubBranchComparison;
  candidates: readonly GithubServiceCandidate[];
}): Result.Result<BranchEvaluationPlan, GithubBranchEvaluationPolicyError> {
  const revision = nextEvaluationRevision(input.cursor);
  if (!revision) {
    return Result.fail(policyError("invalid_input"));
  }
  if (input.liveBranch.state === "absent") {
    return Result.succeed({
      kind: "deleted",
      branch: {
        state: "deleted",
        evaluationReason: "branch_deleted",
        evaluationRevision: revision,
      },
      outcome: "branch_deleted",
      triggers: [],
    });
  }
  if (
    Result.isFailure(
      Schema.decodeUnknownResult(githubExactShaSchema)(
        input.liveBranch.headSha,
      ),
    )
  ) {
    return Result.fail(policyError("invalid_input"));
  }
  if (
    input.cursor?.state === "active" &&
    input.cursor.headSha === input.liveBranch.headSha
  ) {
    return Result.succeed({ kind: "stale", outcome: "ignored_stale" });
  }
  if (
    Result.isFailure(
      Schema.decodeUnknownResult(Schema.Array(githubServiceCandidateSchema))(
        input.candidates,
      ),
    )
  ) {
    return Result.fail(policyError("invalid_input"));
  }

  let selection: GithubEnvironmentTriggerSelection;
  let changedPaths: string[] = [];

  if (!input.cursor || input.cursor.state === "deleted") {
    selection = { mode: "all_services", reason: "first_observation" };
  } else if (input.forced) {
    selection = { mode: "all_services", reason: "force_rebaseline" };
  } else if (input.comparison.state === "not_found") {
    selection = { mode: "all_services", reason: "non_ancestor_rebaseline" };
  } else if (input.comparison.state === "observed") {
    const comparison = input.comparison.value;
    if (
      comparison.baseSha !== input.cursor.headSha ||
      comparison.headSha !== input.liveBranch.headSha
    ) {
      return Result.fail(policyError("invalid_input"));
    }
    if (comparison.status === "ahead" && comparison.pathsComplete) {
      const paths = Schema.decodeUnknownResult(githubChangedPathsSchema)(
        comparison.changedPaths,
      );
      if (Result.isFailure(paths)) {
        return Result.fail(policyError("invalid_input"));
      }
      selection = { mode: "paths", reason: "changed_paths" };
      changedPaths = [...paths.success];
    } else if (!comparison.pathsComplete) {
      selection = {
        mode: "all_services",
        reason: "changed_paths_incomplete_rebaseline",
      };
    } else {
      selection = { mode: "all_services", reason: "non_ancestor_rebaseline" };
    }
  } else {
    return Result.fail(policyError("comparison_required"));
  }

  const triggers = selectGithubEnvironmentTriggers({
    candidates: input.candidates,
    selection,
    changedPaths,
  });
  if (Result.isFailure(triggers)) return Result.fail(triggers.failure);
  const semantics = activePlanSemantics[selection.reason];
  return Result.succeed({
    kind: "active",
    branch: {
      state: "active",
      headSha: input.liveBranch.headSha,
      evaluationReason: semantics.evaluationReason,
      evaluationRevision: revision,
    },
    selection,
    changedPaths,
    triggers: triggers.success,
    outcome: githubBranchEvaluationOutcome(selection, triggers.success.length),
  });
}
