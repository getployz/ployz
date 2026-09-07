import { describe, expect, it } from "vitest";
import { Result } from "effect";
import { planGithubBranchEvaluation } from "#/modules/github/github-branch-evaluation";

const live = {
  state: "present" as const,
  ref: "refs/heads/main",
  headSha: "b".repeat(40),
};

const cursor = {
  state: "active" as const,
  headSha: "a".repeat(40),
  evaluationReason: "changed_paths" as const,
  evaluationRevision: 7,
  lastDeliveryId: "delivery-7",
  lastReceiptSequence: 7,
};

const candidates = [
  {
    environmentId: "env-b",
    serviceId: "svc-z",
    watchPaths: ["services/api/**"],
  },
  {
    environmentId: "env-b",
    serviceId: "svc-a",
    watchPaths: ["services/**"],
  },
  {
    environmentId: "env-a",
    serviceId: "svc-no-match",
    watchPaths: ["docs/**"],
  },
];

describe("GitHub branch evaluation policy", () => {
  it("couples changed-path selection, reason, outcome, paths, and triggers", () => {
    const result = planGithubBranchEvaluation({
      cursor,
      liveBranch: live,
      forced: false,
      comparison: {
        state: "observed",
        value: {
          status: "ahead",
          baseSha: cursor.headSha,
          headSha: live.headSha,
          changedPaths: ["services/api/src/index.ts"],
          pathsComplete: true,
        },
      },
      candidates,
    });

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) return;
    expect(result.success).toEqual({
      kind: "active",
      branch: {
        state: "active",
        headSha: live.headSha,
        evaluationReason: "changed_paths",
        evaluationRevision: 8,
      },
      selection: { mode: "paths", reason: "changed_paths" },
      changedPaths: ["services/api/src/index.ts"],
      outcome: "branch_projected",
      triggers: [
        {
          environmentId: "env-b",
          serviceIds: ["svc-a", "svc-z"],
          selection: { mode: "paths", reason: "changed_paths" },
        },
      ],
    });
  });

  it.each([
    {
      name: "first observation",
      inputCursor: null,
      forced: false,
      comparison: { state: "not_required" as const },
      selection: { mode: "all_services", reason: "first_observation" },
      evaluationReason: "first_observation",
      outcome: "branch_projected",
      changedPaths: [],
      triggerCount: 2,
    },
    {
      name: "changed paths",
      inputCursor: cursor,
      forced: false,
      comparison: {
        state: "observed" as const,
        value: {
          status: "ahead" as const,
          baseSha: cursor.headSha,
          headSha: live.headSha,
          changedPaths: ["services/api/src/index.ts"],
          pathsComplete: true,
        },
      },
      selection: { mode: "paths", reason: "changed_paths" },
      evaluationReason: "changed_paths",
      outcome: "branch_projected",
      changedPaths: ["services/api/src/index.ts"],
      triggerCount: 1,
    },
    {
      name: "forced rebaseline",
      inputCursor: cursor,
      forced: true,
      comparison: { state: "not_required" as const },
      selection: { mode: "all_services", reason: "force_rebaseline" },
      evaluationReason: "rebaseline_all_services",
      outcome: "branch_rebased_all_services",
      changedPaths: [],
      triggerCount: 2,
    },
    {
      name: "non-ancestor rebaseline",
      inputCursor: cursor,
      forced: false,
      comparison: { state: "not_found" as const },
      selection: {
        mode: "all_services",
        reason: "non_ancestor_rebaseline",
      },
      evaluationReason: "rebaseline_all_services",
      outcome: "branch_rebased_all_services",
      changedPaths: [],
      triggerCount: 2,
    },
    {
      name: "incomplete changed-path rebaseline",
      inputCursor: cursor,
      forced: false,
      comparison: {
        state: "observed" as const,
        value: {
          status: "ahead" as const,
          baseSha: cursor.headSha,
          headSha: live.headSha,
          changedPaths: ["services/api/src/index.ts"],
          pathsComplete: false,
        },
      },
      selection: {
        mode: "all_services",
        reason: "changed_paths_incomplete_rebaseline",
      },
      evaluationReason: "rebaseline_all_services",
      outcome: "branch_rebased_all_services",
      changedPaths: [],
      triggerCount: 2,
    },
  ])(
    "derives $name plans and no-trigger outcomes from selection",
    ({
      inputCursor,
      forced,
      comparison,
      selection,
      evaluationReason,
      outcome,
      changedPaths,
      triggerCount,
    }) => {
      const withTriggers = planGithubBranchEvaluation({
        cursor: inputCursor,
        liveBranch: live,
        forced,
        comparison,
        candidates,
      });
      const withoutTriggers = planGithubBranchEvaluation({
        cursor: inputCursor,
        liveBranch: live,
        forced,
        comparison,
        candidates: [],
      });

      expect(Result.isSuccess(withTriggers)).toBe(true);
      expect(Result.isSuccess(withoutTriggers)).toBe(true);
      if (
        Result.isFailure(withTriggers) ||
        Result.isFailure(withoutTriggers)
      ) {
        return;
      }
      expect(withTriggers.success).toMatchObject({
        kind: "active",
        branch: { evaluationReason },
        selection,
        changedPaths,
        outcome,
      });
      if (withTriggers.success.kind !== "active") return;
      expect(withTriggers.success.triggers).toHaveLength(triggerCount);
      expect(withoutTriggers.success).toMatchObject({
        kind: "active",
        branch: { evaluationReason },
        selection,
        changedPaths,
        outcome: "ignored_no_matching_service",
        triggers: [],
      });
    },
  );

  it("returns only legal stale and deleted plans", () => {
    const stale = planGithubBranchEvaluation({
      cursor: { ...cursor, headSha: live.headSha },
      liveBranch: live,
      forced: false,
      comparison: { state: "not_required" },
      candidates,
    });
    const deleted = planGithubBranchEvaluation({
      cursor,
      liveBranch: { state: "absent", ref: live.ref },
      forced: false,
      comparison: { state: "not_required" },
      candidates,
    });

    expect(Result.isSuccess(stale) && stale.success).toEqual({
      kind: "stale",
      outcome: "ignored_stale",
    });
    expect(Result.isSuccess(deleted) && deleted.success).toEqual({
      kind: "deleted",
      branch: {
        state: "deleted",
        evaluationReason: "branch_deleted",
        evaluationRevision: 8,
      },
      outcome: "branch_deleted",
      triggers: [],
    });
  });
});
