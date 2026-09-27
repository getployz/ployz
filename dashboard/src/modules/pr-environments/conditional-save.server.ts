import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { environment, environmentBranch } from "#/modules/project/tables";
import { getEnvironmentContextForActorById } from "#/modules/environment-design/authoring-repository.server";
import { lockBranchScope } from "#/modules/environment-design/workspace-repository.server";
import { loadCurrentEnvironmentState, loadEnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { withoutSealedCiphertext, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadAppliedIntent } from "#/modules/environment-design/saved-state-operations.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { loadIdentitySources } from "#/modules/branches/branch-operations.server";
import { corePicks, sealValue } from "#/modules/branches/branch-merge.server";
import { branchHostnameSuffix } from "#/modules/branches/branch-plan";
import { mergeInput, rowLineage } from "#/modules/branches/branch-review";
import { requestPrCheck } from "./pr-check-request.server";
import { prDestinations } from "./pr-environment.repository.server";
import { standing, type ApproveConditionalSave, type GiveConditionalSaveValue, type WithdrawConditionalSave } from "./conditional-save";
import { conditionalSave, type HeldRow } from "./tables";

/** The PR Environment, checked against the actor's organization. */
const prEnvironmentFor = Effect.fn("PrEnvironments.prEnvironmentFor")(function* (actor: Actor, input: { organizationSlug: string; prEnvironmentId: string }) {
  const context = yield* getEnvironmentContextForActorById(actor, { organizationSlug: input.organizationSlug, environmentId: input.prEnvironmentId });
  if (context === null) return yield* new NotFound({ message: "The PR environment was not found." });
  return context;
});

/**
 * Approve the ticked rows of a PR Environment's changes for one Destination: the Conditional Save, held there until the
 * pull request merges. The rows are recomputed from authoritative states exactly as the browser did; a different review
 * string is refused. It stores what landing needs, so landing never reads the PR Environment. Approving again replaces it.
 */
export const approveConditionalSave = Effect.fn("PrEnvironments.approveConditionalSave")(function* (actor: Actor, input: ApproveConditionalSave) {
  const { project, environment: prEnvironment } = yield* prEnvironmentFor(actor, input);
  const encryption = yield* SecretEncryption;
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // The Project, the Branch row, then the PR Environment's document, whose revision the approval records.
    const branch = yield* lockBranchScope(project.id, input.prEnvironmentId, "share");
    if (branch?.prNumber == null || branch.prRepositoryId === null || branch.prTargetBranch === null) {
      return yield* new NotFound({ message: "The PR environment was not found." });
    }
    const document = yield* loadEnvironmentDocument(input.prEnvironmentId, true);
    if (!(yield* prDestinations(input.prEnvironmentId)).includes(input.destinationEnvironmentId)) {
      return yield* new Conflict({ message: `Nothing there deploys ${branch.prTargetBranch} any more. Review again.` });
    }

    const { from, into, compare } = yield* goesToComparison({
      projectSlug: project.slug, prEnvironment, branch, destinationEnvironmentId: input.destinationEnvironmentId,
    });
    const { rows, review } = branchChanges(compare);
    if (review !== input.review) return yield* new Conflict({ message: "Changes moved after this review. Review them again." });
    const byKey = new Map(rows.flatMap((row) => row.role === "move" ? [[row.key, row] as const] : []));
    const unknown = input.picks.find((pick) => !byKey.has(pick.key));
    if (unknown) return yield* new Validation({ field: "picks", message: `${unknown.key} isn't one of the changes.` });
    if (input.picks.length === 0) return yield* new Validation({ field: "picks", message: "Tick a change to approve." });

    const picks = corePicks({ encryption, rows, into, picks: input.picks });
    const held = input.picks.flatMap((pick): HeldRow[] => {
      const row = byKey.get(pick.key);
      return row ? [{ row: withoutSealedCiphertext(row), option: pick.option, missing: pick.option === "new" && pick.value === "" }] : [];
    });
    const values = {
      organizationId: project.organizationId, projectId: project.id,
      prEnvironmentId: input.prEnvironmentId, repositoryId: branch.prRepositoryId, prNumber: branch.prNumber,
      destinationEnvironmentId: input.destinationEnvironmentId,
      rows: held, picks,
      approvedAgainst: Object.fromEntries(held.map(({ row }) => [row.key, row.into])),
      landing: {
        base: branch.base, from, parent: compare.parent ?? null, hostnames: compare.hostnames,
        identities: yield* loadIdentitySources(input.prEnvironmentId, new Set(picks.map(rowLineage))),
      },
      workingRevision: document.revision, targetBranch: branch.prTargetBranch,
      approvedByUserId: actor.userId, approvedAt: new Date(),
    };
    const [saved] = yield* drizzle.insert(conditionalSave).values(values)
      .onConflictDoUpdate({ target: [conditionalSave.prEnvironmentId, conditionalSave.destinationEnvironmentId], set: values })
      .returning({ id: conditionalSave.id });
    if (!saved) return yield* Effect.die("PostgreSQL did not return the Conditional Save.");
    yield* requestPrCheck([input.prEnvironmentId]);
    return saved;
  }));
});

/** Undo: the approval is withdrawn. */
export const withdrawConditionalSave = Effect.fn("PrEnvironments.withdrawConditionalSave")(function* (actor: Actor, input: WithdrawConditionalSave) {
  yield* prEnvironmentFor(actor, input);
  const { drizzle } = yield* Database;
  yield* drizzle.delete(conditionalSave).where(heldOn(input));
  yield* requestPrCheck([input.prEnvironmentId]);
});

/** A new value an approved row still lacks, sealed and stored with the approval, which stays standing. */
export const giveConditionalSaveValue = Effect.fn("PrEnvironments.giveConditionalSaveValue")(function* (actor: Actor, input: GiveConditionalSaveValue) {
  yield* prEnvironmentFor(actor, input);
  const encryption = yield* SecretEncryption;
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [save] = yield* drizzle.select().from(conditionalSave).where(heldOn(input)).for("update");
    const [pr] = yield* drizzle.select({ id: environment.id, revision: environment.revision, targetBranch: environmentBranch.prTargetBranch })
      .from(environment).innerJoin(environmentBranch, eq(environmentBranch.environmentId, environment.id))
      .where(eq(environment.id, input.prEnvironmentId));
    if (!save || !standing(save, pr)) return yield* new Conflict({ message: "It isn't approved any more. Review and approve again." });
    const held = save.rows.find(({ row }) => row.key === input.key);
    if (!held || held.option !== "new") return yield* new Validation({ field: "key", message: "That change doesn't take a new value." });
    const { intent: into } = yield* loadEnvironmentDocument(input.destinationEnvironmentId);
    const value = sealValue(encryption, [held.row], into, input.key, input.value);
    yield* drizzle.update(conditionalSave).set({
      picks: save.picks.map((pick) => pick.key === input.key ? { key: pick.key, choice: { option: "new", value } } : pick),
      rows: save.rows.map((row) => row.row.key === input.key ? { ...row, missing: false } : row),
    }).where(eq(conditionalSave.id, save.id));
    yield* requestPrCheck([input.prEnvironmentId]);
    return { id: save.id };
  }));
});

/**
 * What a PR Environment moves into one Destination, compared from authoritative states exactly as the review does in
 * the browser: its Working State into the Destination's, over its base, with the Parent's Applied State as "the
 * Parent's value".
 */
export const goesToComparison = Effect.fn("PrEnvironments.goesToComparison")(function* (input: {
  projectSlug: string;
  prEnvironment: { id: string; namespace: string };
  branch: { base: SavedEnvironmentIntent; parentEnvironmentId: string };
  destinationEnvironmentId: string;
}) {
  const { drizzle } = yield* Database;
  const { intent: from } = yield* loadCurrentEnvironmentState(input.prEnvironment.id);
  const { intent: into } = yield* loadCurrentEnvironmentState(input.destinationEnvironmentId);
  const [destination] = yield* drizzle.select({ namespace: environment.namespace, branch: environmentBranch.environmentId }).from(environment)
    .leftJoin(environmentBranch, eq(environmentBranch.environmentId, environment.id))
    .where(eq(environment.id, input.destinationEnvironmentId));
  if (!destination) return yield* new NotFound({ message: "The destination was not found." });
  const [parent] = yield* drizzle.select({ namespace: environment.namespace }).from(environment).where(eq(environment.id, input.branch.parentEnvironmentId));
  const compare = mergeInput({
    base: input.branch.base, kept: false, branch: from, parent: into,
    parentApplied: parent ? yield* appliedIntent(input.branch.parentEnvironmentId, parent.namespace) : null,
    hostnames: {
      branch: branchHostnameSuffix(input.projectSlug, input.prEnvironment.namespace, true),
      parent: branchHostnameSuffix(input.projectSlug, destination.namespace, destination.branch !== null),
    },
  });
  return { from, into, compare };
});

const heldOn = (input: { prEnvironmentId: string; destinationEnvironmentId: string }) => and(
  eq(conditionalSave.prEnvironmentId, input.prEnvironmentId), eq(conditionalSave.destinationEnvironmentId, input.destinationEnvironmentId),
);

/** An Environment's Applied State in authored form; null before its first deploy. */
const appliedIntent = Effect.fn("PrEnvironments.appliedIntent")(function* (environmentId: string, namespace: string) {
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId });
  const deployed = projection.explicitStates.find((state) => state.environmentId === environmentId)?.applied.nodes.length ?? 0;
  return deployed ? yield* loadAppliedIntent(environmentId, namespace, projection) : null;
});
