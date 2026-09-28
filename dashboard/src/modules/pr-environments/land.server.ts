import "@tanstack/react-start/server-only";
import { isDeepStrictEqual } from "node:util";
import { and, asc, desc, eq, inArray, ne, sql } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges, type BranchRow } from "@ployz/sdk/config";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { type environment, project } from "#/modules/project/tables";
import { githubBranchProjection, githubEnvironmentTrigger } from "#/modules/github/tables";
import {
  compareInstallationRepositoryCommits, fetchInstallationPullRequest, isGithubObservationNotFound, resolveGithubRepository,
  type GithubResolvedRepository,
} from "#/modules/github/github-observation.api";
import { service } from "#/modules/environment-design/tables";
import { parseDashboardEnvironmentIntent, withoutSealedCiphertext, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadCurrentEnvironmentState, loadEnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { loadLatestEnvironmentSavedState } from "#/modules/environment-design/saved-state-repository.server";
import { publishLandedSavedState } from "#/modules/environment-design/saved-state-operations.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { core, landChanges } from "#/modules/branches/branch-operations.server";
import { withEmptyValues } from "#/modules/branches/branch-merge.server";
import { rowLineage, usedLive } from "#/modules/branches/branch-review";
import type { CarriedSave } from "./carried";
import { actingMember } from "./plan-operations.server";
import { trackedBranch } from "./pull-request";
import { prDestinations } from "./pr-environment.repository.server";
import { conditionalSave, prEnvironment, prEnvironmentPlan } from "./tables";

type ConditionalSave = typeof conditionalSave.$inferSelect;
type Document = typeof environment.$inferSelect;

const isNode = (pick: { key: string }) => pick.key.endsWith(":node");
const same = (left: BranchRow["into"] | undefined, right: BranchRow["into"] | undefined) => isDeepStrictEqual(left ?? null, right ?? null);
const frozen = eq(conditionalSave.state, "frozen");

/**
 * Lands a frozen Conditional Save in its Destination, `document`, without reading the PR Environment. The caller holds
 * the Destination's queue lock, then its document (`loadEnvironmentDocument(id, true)`), as GitHub admission does.
 *
 * A picked setting is saved unless the Destination changed it since approval, when neither its Saved nor its Working
 * State still has the value the review showed: that one isn't saved, and is marked; it's staged instead where Working
 * has no staged edit of its own, which it keeps. Arriving nodes and variables get fresh identities, the same ones in
 * both. A missing value arrives empty. Afterwards the Conditional Save is gone, or `landed`, marking what wasn't saved
 * until the Destination's next Saved revision. Returns the Saved revision landing published, and the document as
 * landing left it.
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
  // Each moving row's value on the receiving side, redacted as the review showed it.
  const valuesIn = (into: SavedEnvironmentIntent) => core("landing", () => new Map(branchChanges(compare(into)).rows
    .flatMap((row) => row.role === "move" ? [[row.key, withoutSealedCiphertext(row).into] as const] : [])));
  const inSaved = yield* valuesIn(latest.intent);
  const inWorking = yield* valuesIn(working);
  const approvedAgainst = new Map(save.rows.map(({ row }) => [row.key, row.into]));
  const unchanged = (key: string) => same(inSaved.get(key), approvedAgainst.get(key)) || same(inWorking.get(key), approvedAgainst.get(key));

  // A missing new value arrives empty (`isEmptyValue`); the service shows it as missing, and nothing blocks.
  const picks = withEmptyValues({ encryption, rows: save.rows.map(({ row }) => row), into: working, picks: save.picks });

  // 1. Saved. A node the pull request introduced arrives with its variables, or not at all.
  const introduced = new Set(picks.filter(isNode).map(rowLineage));
  const toSaved = picks.filter((pick) => inSaved.has(pick.key) && unchanged(pick.key));
  const arriving = new Set(toSaved.filter(isNode).map(rowLineage));
  const savedPicks = toSaved.filter((pick) => !introduced.has(rowLineage(pick)) || arriving.has(rowLineage(pick)));
  const saved = parseDashboardEnvironmentIntent((yield* core("picks", () => branchChanges({ ...compare(latest.intent), picks: savedPicks }))).next);

  // 2. Working: arriving nodes as Saved has them, so their ids match; then the rest, where the Destination has no
  // staged edit of its own, with the variables Saved gained under Saved's ids.
  const into = {
    ...working,
    services: [...working.services, ...saved.services.filter((node) => arriving.has(node.lineageId))],
    volumes: [...working.volumes, ...saved.volumes.filter((node) => arriving.has(node.resourceLineageId))],
  };
  const workingPicks = picks.filter((pick) => !introduced.has(rowLineage(pick))
    && inWorking.has(pick.key) && same(inSaved.get(pick.key), inWorking.get(pick.key)));
  const next = withVariableIdsOf(saved, into,
    parseDashboardEnvironmentIntent((yield* core("picks", () => branchChanges({ ...compare(into), picks: workingPicks }))).next));

  // 3. Identities for what arrives, both states, then what's left of the Conditional Save.
  const [owner] = yield* drizzle.select().from(project).where(eq(project.id, save.projectId));
  if (!owner) return yield* Effect.die("The Destination's project is missing.");
  const published = yield* publishLandedSavedState({
    environmentId: document.id, actorId: yield* actorFor(save), message: `Merge #${save.prNumber}`,
    basis: { kind: "saved_revision", savedStateSnapshotId: latest.id }, intent: saved,
  });
  const written = yield* landChanges({ project: owner, sources: save.landing.identities, document, into: working, next, picks: [...savedPicks, ...workingPicks] });
  const notSaved = save.rows.filter(({ row }) => !unchanged(row.key));
  yield* drizzle.delete(conditionalSave).where(and(eq(conditionalSave.destinationEnvironmentId, document.id),
    eq(conditionalSave.state, "landed"), ne(conditionalSave.id, save.id)));
  if (notSaved.length === 0) yield* drizzle.delete(conditionalSave).where(eq(conditionalSave.id, save.id));
  else {
    yield* drizzle.update(conditionalSave).set({ state: "landed", rows: notSaved, landedSavedStateId: published.savedStateSnapshotId })
      .where(eq(conditionalSave.id, save.id));
  }
  return { savedStateSnapshotId: published.savedStateSnapshotId, document: written };
});

/** `next` with each variable `before` lacked under the id `saved` gave it (by service lineage and key), so both states agree. */
function withVariableIdsOf(saved: SavedEnvironmentIntent, before: SavedEnvironmentIntent, next: SavedEnvironmentIntent): SavedEnvironmentIntent {
  return {
    ...next,
    services: next.services.map((node) => {
      const had = new Set(before.services.find((own) => own.id === node.id)?.variables.map((variable) => variable.id));
      const inSaved = saved.services.find((own) => own.lineageId === node.lineageId)?.variables ?? [];
      return {
        ...node,
        variables: node.variables.map((variable) => had.has(variable.id) ? variable
          : { ...variable, id: inSaved.find((own) => own.key === variable.key)?.id ?? variable.id }),
      };
    }),
  };
}

/** Who the landed Saved revision is by: the approver, or, once they're gone, whom PR Environments act as. */
const actorFor = Effect.fn("PrEnvironments.actorFor")(function* (save: ConditionalSave) {
  if (save.approvedByUserId) return save.approvedByUserId;
  const { drizzle } = yield* Database;
  const [plan] = yield* drizzle.select().from(prEnvironmentPlan)
    .where(and(eq(prEnvironmentPlan.projectId, save.projectId), eq(prEnvironmentPlan.repositoryId, save.repositoryId)));
  const userId = plan && (yield* actingMember(plan));
  return userId ?? (yield* Effect.die("Nobody is left to land the held changes as."));
});

/**
 * Whether the Destination deploys the pull request's target Git branch on push: a service from the repository that tracks
 * it in its latest Saved State and has Auto-deploy on. Its held changes then wait for the merge commit's deployment.
 */
const deploysOnPush = Effect.fn("PrEnvironments.deploysOnPush")(function* (save: ConditionalSave) {
  const { drizzle } = yield* Database;
  const latest = yield* loadLatestEnvironmentSavedState(save.destinationEnvironmentId);
  const tracking = (latest?.intent.services ?? []).filter(({ config }) => trackedBranch(config, save.repositoryId) === save.targetBranch);
  if (tracking.length === 0) return false;
  const rows = yield* drizzle.select({ policy: service.policy }).from(service).where(inArray(service.id, tracking.map((node) => node.id)));
  return rows.some((row) => row.policy.autoDeploy);
});

/**
 * Lands frozen Conditional Save `id` now, in its own transaction under its Destination's queue lock, when `now` agrees.
 * Returns false only when `now` refused; a save no longer frozen landed elsewhere.
 */
const landNow = <E, R>(id: string, destinationId: string, now: (save: ConditionalSave) => Effect.Effect<boolean, E, R>) => Effect.gen(function* () {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* lockEnvironmentDeploymentQueue(destinationId);
    const document = yield* loadEnvironmentDocument(destinationId, true);
    const [save] = yield* drizzle.select().from(conditionalSave).where(and(eq(conditionalSave.id, id), frozen)).for("update");
    if (!save) return true;
    if (!(yield* now(save))) return false;
    yield* landConditionalSave(save, document);
    return true;
  }));
}).pipe(Effect.withSpan("PrEnvironments.landNow"));

const ofPullRequest = (pullRequest: { repositoryId: number; number: number }) =>
  and(eq(conditionalSave.repositoryId, pullRequest.repositoryId), eq(conditionalSave.prNumber, pullRequest.number), frozen);

/**
 * Lands each of the merged pull request's frozen Conditional Saves whose Destination doesn't deploy on push, each in its
 * own transaction. The rest wait for the merge commit's deployment.
 */
export const landAtMerge = Effect.fn("PrEnvironments.landAtMerge")(function* (pullRequest: { repositoryId: number; number: number }) {
  const { drizzle } = yield* Database;
  const saves = yield* drizzle.select({ id: conditionalSave.id, destinationId: conditionalSave.destinationEnvironmentId })
    .from(conditionalSave).where(ofPullRequest(pullRequest));
  for (const { id, destinationId } of saves) yield* landNow(id, destinationId, (save) => deploysOnPush(save).pipe(Effect.map((deploys) => !deploys)));
});

/** Whether `sha` is, or descends from, `ancestor`. */
const descendsFrom = (installationId: number, repository: GithubResolvedRepository, ancestor: string, sha: string) => ancestor === sha
  ? Effect.succeed(true)
  : compareInstallationRepositoryCommits(installationId, repository, ancestor, sha).pipe(
    Effect.map(({ status }) => status === "ahead" || status === "identical"),
    Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(false)),
  );

type Push = { installationId: number; repository: GithubResolvedRepository; repositoryId: number; ref: string; headSha: string };
const ofTargetBranch = (push: Push) =>
  and(eq(conditionalSave.repositoryId, push.repositoryId), eq(conditionalSave.targetBranch, push.ref.slice("refs/heads/".length)));

/**
 * Pull requests that merged before their closed delivery arrived: their approvals freeze now, so a push carrying their
 * merge commits carries them, however many merged before it. Asks GitHub about each pull request with an approval
 * standing on the pushed Git branch, only when one stands.
 */
export const freezeMergedBy = Effect.fn("PrEnvironments.freezeMergedBy")(function* (push: Push) {
  const { drizzle } = yield* Database;
  const standing = yield* drizzle.selectDistinct({ number: conditionalSave.prNumber }).from(conditionalSave)
    .where(and(ofTargetBranch(push), eq(conditionalSave.state, "standing")));
  const targetBranch = push.ref.slice("refs/heads/".length);
  for (const { number } of standing) {
    const pull = yield* fetchInstallationPullRequest(push.installationId, push.repositoryId, number);
    if (!pull.mergeCommitSha || pull.targetBranch !== targetBranch) continue;
    const prEnvironments = yield* drizzle.select({ id: prEnvironment.environmentId }).from(prEnvironment)
      .where(and(eq(prEnvironment.repositoryId, push.repositoryId), eq(prEnvironment.number, number), eq(prEnvironment.retired, false)));
    // One being torn down is being retired: its approvals drop rather than freeze.
    const closing = yield* activeTeardownFor(prEnvironments.map((row) => row.id));
    yield* settleAtClose(prEnvironments.map((row) => row.id).filter((id) => !closing.has(id)), { commitSha: pull.mergeCommitSha, targetBranch });
  }
});

/**
 * The frozen Conditional Saves a push of `headSha` to `ref` carries: those whose merge commit it is, or descends from.
 * Asks GitHub, so call it before the admission transaction.
 */
export const heldChangesCarriedBy = Effect.fn("PrEnvironments.heldChangesCarriedBy")(function* (push: Push) {
  const { drizzle } = yield* Database;
  const saves = yield* drizzle.select().from(conditionalSave).where(and(ofTargetBranch(push), frozen));
  const carried: CarriedSave[] = [];
  const descends = new Map<string, boolean>();
  for (const save of saves) {
    const merge = mergeCommitOf(save);
    if (!descends.has(merge)) descends.set(merge, yield* descendsFrom(push.installationId, push.repository, merge, push.headSha));
    if (descends.get(merge)) carried.push({ id: save.id, destinationEnvironmentId: save.destinationEnvironmentId });
  }
  return carried;
});

/** A frozen Conditional Save's merge commit; the state check guarantees one. */
const mergeCommitOf = (save: ConditionalSave) => save.mergeCommitSha ?? "";

/**
 * Lands the frozen Conditional Saves among `ids` held on `document`, a Destination whose queue lock and document the
 * caller holds. Returns whether any landed, and the document as landing left it.
 */
export const landCarried = Effect.fn("PrEnvironments.landCarried")(function* (ids: readonly string[], document: Document) {
  if (ids.length === 0) return { landed: false, document };
  const { drizzle } = yield* Database;
  const saves = yield* drizzle.select().from(conditionalSave)
    .where(and(inArray(conditionalSave.id, [...ids]), eq(conditionalSave.destinationEnvironmentId, document.id), frozen))
    .orderBy(asc(conditionalSave.approvedAt)).for("update");
  let current = document;
  for (const save of saves) current = (yield* landConditionalSave(save, current)).document;
  return { landed: saves.length > 0, document: current };
});

/** Each carried Destination the push deploys nothing in saves what it carries now: it takes the queue lock itself. */
export const landCarriedInIdle = Effect.fn("PrEnvironments.landCarriedInIdle")(function* (
  carried: readonly CarriedSave[], deploying: ReadonlySet<string>,
) {
  const idle = new Set(carried.map((save) => save.destinationEnvironmentId).filter((id) => !deploying.has(id)));
  for (const environmentId of idle) {
    yield* lockEnvironmentDeploymentQueue(environmentId);
    yield* landCarried(carried.map((save) => save.id), yield* loadEnvironmentDocument(environmentId, true));
  }
});

/** The newest trigger for `save`'s Destination on its target Git branch that isn't superseded. */
const latestTriggerFor = Effect.fn("PrEnvironments.latestTriggerFor")(function* (save: ConditionalSave) {
  const { drizzle } = yield* Database;
  const [latest] = yield* drizzle.select().from(githubEnvironmentTrigger).where(and(
    eq(githubEnvironmentTrigger.repositoryId, save.repositoryId), eq(githubEnvironmentTrigger.ref, `refs/heads/${save.targetBranch}`),
    eq(githubEnvironmentTrigger.environmentId, save.destinationEnvironmentId), ne(githubEnvironmentTrigger.admissionState, "superseded"),
  )).orderBy(desc(githubEnvironmentTrigger.createdAt)).limit(1);
  return latest ?? null;
});

/**
 * The merged pull request's frozen Conditional Saves the pushes so far already carry, once its target Git branch's
 * processed head has the merge commit (else the next push that has it carries them). One rides a trigger waiting for
 * CI in its Destination: admitted, it lands them; superseded, it hands them on (`handOverCarried`). With nothing
 * waiting there, the Destination has taken every processed push, so they're saved now. Asks GitHub, outside any
 * transaction.
 */
export const carryInWaitingTriggers = Effect.fn("PrEnvironments.carryInWaitingTriggers")(function* (pullRequest: {
  installationId: number; repositoryId: number; number: number;
}) {
  const { drizzle } = yield* Database;
  const saves = yield* drizzle.select().from(conditionalSave).where(ofPullRequest(pullRequest));
  let repository: GithubResolvedRepository | undefined;
  for (const save of saves) {
    const [branch] = yield* drizzle.select({ head: githubBranchProjection.evaluatedHeadSha }).from(githubBranchProjection).where(and(
      eq(githubBranchProjection.installationId, pullRequest.installationId), eq(githubBranchProjection.repositoryId, save.repositoryId),
      eq(githubBranchProjection.ref, `refs/heads/${save.targetBranch}`)));
    if (!branch?.head) continue;
    repository ??= yield* resolveGithubRepository(pullRequest.installationId, pullRequest.repositoryId);
    if (!(yield* descendsFrom(pullRequest.installationId, repository, mergeCommitOf(save), branch.head))) continue;
    // ponytail: a few looks, as triggers are admitted or superseded meanwhile; past that the next push carries them.
    for (let look = 0; look < 3; look++) {
      const latest = yield* latestTriggerFor(save);
      if (latest?.conditionalSaveIds.includes(save.id)) break;
      if (latest?.admissionState === "waiting") {
        const [attached] = yield* drizzle.update(githubEnvironmentTrigger)
          .set({ conditionalSaveIds: sql`array_append(${githubEnvironmentTrigger.conditionalSaveIds}, ${save.id}::uuid)` })
          .where(and(eq(githubEnvironmentTrigger.id, latest.id), eq(githubEnvironmentTrigger.admissionState, "waiting")))
          .returning({ id: githubEnvironmentTrigger.id });
        if (attached) break;
        continue;
      }
      // Under the queue lock nothing may wait for CI there by now, or that trigger carries them instead.
      if (yield* landNow(save.id, save.destinationEnvironmentId, () => latestTriggerFor(save).pipe(
        Effect.map((now) => now?.admissionState !== "waiting")))) break;
    }
  }
});

/**
 * A trigger that carries frozen Conditional Saves is superseded unadmitted: they move to the trigger still waiting on
 * its Git branch in the same Destination, or, with none, land now in `document`, whose queue lock and document the
 * caller holds, as close-time landing does. Either way nothing it carried is left behind.
 */
export const handOverCarried = Effect.fn("PrEnvironments.handOverCarried")(function* (
  trigger: { id: string; environmentId: string; repositoryId: number; ref: string; conditionalSaveIds: string[] }, document: Document,
) {
  if (trigger.conditionalSaveIds.length === 0) return;
  const { drizzle } = yield* Database;
  const [waiting] = yield* drizzle.select().from(githubEnvironmentTrigger).where(and(
    eq(githubEnvironmentTrigger.environmentId, trigger.environmentId), eq(githubEnvironmentTrigger.repositoryId, trigger.repositoryId),
    eq(githubEnvironmentTrigger.ref, trigger.ref), eq(githubEnvironmentTrigger.admissionState, "waiting"), ne(githubEnvironmentTrigger.id, trigger.id),
  )).orderBy(desc(githubEnvironmentTrigger.createdAt)).limit(1).for("update");
  const [next] = waiting ? yield* drizzle.update(githubEnvironmentTrigger)
    .set({ conditionalSaveIds: [...new Set([...waiting.conditionalSaveIds, ...trigger.conditionalSaveIds])] })
    .where(eq(githubEnvironmentTrigger.id, waiting.id)).returning({ id: githubEnvironmentTrigger.id }) : [];
  if (!next) yield* landCarried(trigger.conditionalSaveIds, document);
});

/**
 * The pull request closed: each PR Environment's Conditional Saves freeze with the merge commit if it merged and they
 * still stand, and drop otherwise. Frozen ones leave the PR Environment, so its teardown keeps them. Call before it.
 */
export const settleAtClose = Effect.fn("PrEnvironments.settleAtClose")(function* (
  prEnvironmentIds: string[], merge: { commitSha: string; targetBranch: string } | null,
) {
  const database = yield* Database;
  for (const prEnvironmentId of prEnvironmentIds) {
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      // Its document, so no settings edit slips in between the check and the freeze.
      const document = yield* loadEnvironmentDocument(prEnvironmentId, true);
      const ofPr = eq(conditionalSave.prEnvironmentId, prEnvironmentId);
      if (merge) {
        // Only where it's still a Destination: anywhere else the approval drops.
        const current = yield* prDestinations(prEnvironmentId);
        yield* drizzle.update(conditionalSave).set({ state: "frozen", mergeCommitSha: merge.commitSha, prEnvironmentId: null })
          .where(and(ofPr, eq(conditionalSave.workingRevision, document.revision), eq(conditionalSave.targetBranch, merge.targetBranch),
            inArray(conditionalSave.destinationEnvironmentId, current)));
      }
      yield* drizzle.delete(conditionalSave).where(ofPr);
      // Closed under its document: settled before the closed delivery (a merge push), it takes no approval after.
      yield* drizzle.update(prEnvironment).set({ closed: true }).where(eq(prEnvironment.environmentId, prEnvironmentId));
    }));
  }
});
