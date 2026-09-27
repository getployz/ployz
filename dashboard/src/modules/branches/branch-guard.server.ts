import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { DatabaseService } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { environmentNodeIntroduction } from "#/modules/runtime/tables";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { buildEnvironmentChangeSet, type EnvironmentNodeIntroductionsProjection, type EnvironmentNodeProjection, type EnvironmentStateProjection } from "#/modules/environment-design/environment-change-set";
import { loadCurrentEnvironmentState } from "#/modules/environment-design/working-state-repository.server";

type Node = { nodeType: string; nodeId: string; config: unknown };
// SAFETY: every projection pairs nodeType with its config; TypeScript cannot correlate the two fields.
const projected = (token: string, nodes: Node[]): EnvironmentStateProjection => ({
  token, nodes: nodes.map((node) => ({ node: { type: node.nodeType, id: node.nodeId }, config: node.config }) as EnvironmentNodeProjection),
});

/**
 * Merge and Update gate: refuses unless the Branch has no active attempt and its Environment Change Set is empty, so
 * Working State equals what it runs. Call it inside the transaction that holds the Branch's deployment queue lock.
 */
export const assertBranchSettled = Effect.fn("Branches.assertBranchSettled")(function* (tx: DatabaseService["drizzle"], branchEnvironmentId: string) {
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: branchEnvironmentId });
  const state = projection.explicitStates.find((candidate) => candidate.environmentId === branchEnvironmentId);
  if (state?.deploymentEvidence) return yield* new Conflict({ message: "This branch is deploying. Try again when it finishes." });
  const working = yield* loadCurrentEnvironmentState(branchEnvironmentId);
  const introductions = yield* tx.select().from(environmentNodeIntroduction).where(eq(environmentNodeIntroduction.environmentId, branchEnvironmentId));
  const changes = buildEnvironmentChangeSet({
    working: projected("working", working.projection.nodeSnapshots),
    applied: projected("applied", state?.applied.nodes ?? []),
    saved: state?.saved ? projected("saved", state.saved.nodes) : null,
    submitted: null,
    // SAFETY: an introduction's config is the node's config for its nodeType.
    nodeIntroductions: projected("introductions", introductions) as EnvironmentNodeIntroductionsProjection,
  });
  if (changes.totalCount > 0) return yield* new Conflict({ message: "This branch has changes it hasn't deployed. Deploy or discard them first." });
});
