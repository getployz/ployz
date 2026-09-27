import "@tanstack/react-start/server-only";
import { isDeepStrictEqual } from "node:util";
import { and, eq, inArray, isNotNull, isNull, ne } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges, type BranchPick } from "@ployz/sdk/config";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { project } from "#/modules/project/tables";
import { service } from "#/modules/environment-design/tables";
import { parseDashboardEnvironmentIntent, withoutSealedCiphertext, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadCurrentEnvironmentState, loadEnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { loadLatestEnvironmentSavedState } from "#/modules/environment-design/saved-state-repository.server";
import { publishLandedSavedState } from "#/modules/environment-design/saved-state-operations.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { core, landChanges } from "#/modules/branches/branch-operations.server";
import { sealValue } from "#/modules/branches/branch-merge.server";
import { rowLineage, usedLive } from "#/modules/branches/branch-review";
import { fromRepository } from "./pull-request";
import { conditionalSave, prEnvironmentPlan } from "./tables";

type ConditionalSave = typeof conditionalSave.$inferSelect;
type Document = Effect.Success<ReturnType<typeof loadEnvironmentDocument>>;

const isNode = (pick: { key: string }) => pick.key.endsWith(":node");
const same = (a: unknown, b: unknown) => isDeepStrictEqual(a ?? null, b ?? null);

/**
 * Lands a frozen Conditional Save in its Destination, `document`, without reading the PR Environment. The caller holds
 * the Destination's queue lock, then its document (`loadEnvironmentDocument(id, true)`), as GitHub admission does.
 *
 * A picked setting is saved unless the Destination changed it since approval, when neither its Saved nor its Working
 * State still has the value approved against: that one is staged instead, and marked. In Working, a setting with a
 * staged edit keeps it. Arriving nodes get fresh identities, the same ones in both. A missing value arrives empty.
 * Afterwards the Conditional Save is gone, or only marks what was staged instead until the Destination's next Saved
 * revision. Returns the Saved revision landing published.
 */
export const landConditionalSave = Effect.fn("PrEnvironments.landConditionalSave")(function* (save: ConditionalSave, document: Document) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const latest = yield* loadLatestEnvironmentSavedState(document.id);
  if (!latest) return yield* Effect.die("A Destination has a Saved State.");
  const { intent: working } = yield* loadCurrentEnvironmentState(document.id);
  const { base, from, parent, hostnames } = save.landing;
  const compare = (into: SavedEnvironmentIntent) => ({
    base, from, into, parent: parent ?? undefined, provided: usedLive(into), hostnames, fromKept: false,
  });
  // Each moving row's value on the receiving side, redacted as `approved_against` is.
  const valuesIn = (into: SavedEnvironmentIntent) => core("landing", () => new Map(branchChanges(compare(into)).rows
    .flatMap((row) => row.role === "move" ? [[row.key, withoutSealedCiphertext(row).into] as const] : [])));
  const inSaved = yield* valuesIn(latest.intent);
  const inWorking = yield* valuesIn(working);
  const unchanged = (key: string) => same(inSaved.get(key), save.approvedAgainst[key]) || same(inWorking.get(key), save.approvedAgainst[key]);

  // A missing new value arrives empty; the service shows it as missing, and nothing blocks.
  const held = save.rows.map(({ row }) => row);
  const picks = save.picks.map((pick): BranchPick => pick.choice?.option === "new" && !pick.choice.value
    ? { key: pick.key, choice: { option: "new", value: sealValue(encryption, held, working, pick.key, "") } } : pick);

  // 1. Saved. A node the pull request introduced arrives with its variables, or not at all.
  const introduced = new Set(picks.filter(isNode).map(rowLineage));
  const toSaved = picks.filter((pick) => inSaved.has(pick.key) && unchanged(pick.key));
  const arriving = new Set(toSaved.filter(isNode).map(rowLineage));
  const savedPicks = toSaved.filter((pick) => !introduced.has(rowLineage(pick)) || arriving.has(rowLineage(pick)));
  const saved = parseDashboardEnvironmentIntent((yield* core("picks", () => branchChanges({ ...compare(latest.intent), picks: savedPicks }))).next);

  // 2. Working: arriving nodes as Saved has them, so their ids match; then the rest, where the Destination has no
  // staged edit of its own.
  const into = {
    ...working,
    services: [...working.services, ...saved.services.filter((node) => arriving.has(node.lineageId))],
    volumes: [...working.volumes, ...saved.volumes.filter((node) => arriving.has(node.resourceLineageId))],
  };
  const workingPicks = picks.filter((pick) => !introduced.has(rowLineage(pick))
    && inWorking.has(pick.key) && same(inSaved.get(pick.key), inWorking.get(pick.key)));
  const next = parseDashboardEnvironmentIntent((yield* core("picks", () => branchChanges({ ...compare(into), picks: workingPicks }))).next);

  // 3. Identities for what arrives, both states, then what's left of the Conditional Save.
  const [owner] = yield* drizzle.select().from(project).where(eq(project.id, save.projectId));
  if (!owner) return yield* Effect.die("The Destination's project is missing.");
  const published = yield* publishLandedSavedState({
    environmentId: document.id, actorId: yield* actorFor(save), message: `Merge #${save.prNumber}`,
    basis: { kind: "saved_revision", savedStateSnapshotId: latest.id }, intent: saved,
  });
  yield* landChanges({ project: owner, sources: save.landing.identities, document, into: working, next, picks: [...savedPicks, ...workingPicks] });
  const stagedInstead = save.rows.filter(({ row }) => !unchanged(row.key) && workingPicks.some((pick) => pick.key === row.key));
  yield* drizzle.delete(conditionalSave).where(and(eq(conditionalSave.destinationEnvironmentId, document.id),
    isNotNull(conditionalSave.landedSavedStateId), ne(conditionalSave.id, save.id)));
  if (stagedInstead.length === 0) yield* drizzle.delete(conditionalSave).where(eq(conditionalSave.id, save.id));
  else {
    yield* drizzle.update(conditionalSave).set({ rows: stagedInstead, landedSavedStateId: published.savedStateSnapshotId })
      .where(eq(conditionalSave.id, save.id));
  }
  return published.savedStateSnapshotId;
});

/** Who the landed Saved revision is by: the approver, or, once they're gone, whoever turned PR Environments on. */
const actorFor = Effect.fn("PrEnvironments.actorFor")(function* (save: ConditionalSave) {
  if (save.approvedByUserId) return save.approvedByUserId;
  const { drizzle } = yield* Database;
  const [plan] = yield* drizzle.select({ userId: prEnvironmentPlan.enabledByUserId }).from(prEnvironmentPlan)
    .where(and(eq(prEnvironmentPlan.projectId, save.projectId), eq(prEnvironmentPlan.repositoryId, save.repositoryId)));
  return plan?.userId ?? (yield* Effect.die("Nobody is left to land the held changes as."));
});

/**
 * Whether the Destination deploys the pull request's target Git branch on push: a service from the repository that tracks
 * it in its latest Saved State and has Auto-deploy on. Its held changes then wait for the merge commit's deployment.
 */
const deploysOnPush = Effect.fn("PrEnvironments.deploysOnPush")(function* (save: ConditionalSave) {
  const { drizzle } = yield* Database;
  const latest = yield* loadLatestEnvironmentSavedState(save.destinationEnvironmentId);
  const tracking = (latest?.intent.services ?? []).filter(({ config }) => fromRepository(config, save.repositoryId)
    && config.source.type === "git" && config.source.branch.type === "connected" && config.source.branch.name === save.targetBranch);
  if (tracking.length === 0) return false;
  const rows = yield* drizzle.select({ policy: service.policy }).from(service).where(inArray(service.id, tracking.map((node) => node.id)));
  return rows.some((row) => row.policy.autoDeploy);
});

/**
 * Lands each of the merged pull request's frozen Conditional Saves whose Destination doesn't deploy on push, each in its
 * own transaction. The rest wait for the merge commit's deployment.
 */
export const landAtMerge = Effect.fn("PrEnvironments.landAtMerge")(function* (pullRequest: { repositoryId: number; number: number }) {
  const database = yield* Database;
  const frozen = yield* database.drizzle.select({ id: conditionalSave.id, destinationId: conditionalSave.destinationEnvironmentId }).from(conditionalSave)
    .where(and(eq(conditionalSave.repositoryId, pullRequest.repositoryId), eq(conditionalSave.prNumber, pullRequest.number),
      isNotNull(conditionalSave.mergeCommitSha), isNull(conditionalSave.landedSavedStateId)));
  for (const { id, destinationId } of frozen) {
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* lockEnvironmentDeploymentQueue(destinationId);
      const document = yield* loadEnvironmentDocument(destinationId, true);
      const [save] = yield* drizzle.select().from(conditionalSave)
        .where(and(eq(conditionalSave.id, id), isNull(conditionalSave.landedSavedStateId))).for("update");
      if (save && !(yield* deploysOnPush(save))) yield* landConditionalSave(save, document);
    }));
  }
});

/**
 * The pull request closed: each PR Environment's Conditional Saves freeze with the merge commit if it merged and they
 * still stand, and drop otherwise. Frozen ones leave the PR Environment, so its teardown keeps them. Call before it.
 */
export const settleAtClose = Effect.fn("PrEnvironments.settleAtClose")(function* (prEnvironmentIds: string[], mergeCommitSha: string | null) {
  const database = yield* Database;
  for (const prEnvironmentId of prEnvironmentIds) {
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      // Its document, so no settings edit slips in between the check and the freeze.
      const document = yield* loadEnvironmentDocument(prEnvironmentId, true);
      const ofPr = eq(conditionalSave.prEnvironmentId, prEnvironmentId);
      if (mergeCommitSha) {
        yield* drizzle.update(conditionalSave).set({ mergeCommitSha, prEnvironmentId: null })
          .where(and(ofPr, eq(conditionalSave.workingRevision, document.revision)));
      }
      yield* drizzle.delete(conditionalSave).where(ofPr);
    }));
  }
});
