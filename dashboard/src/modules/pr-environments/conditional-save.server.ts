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
import { core, loadIdentitySources } from "#/modules/branches/branch-operations.server";
import { corePicks } from "#/modules/branches/branch-save.server";
import { branchHostnameSuffix } from "#/modules/branches/branch-plan";
import { saveInput, rowLineage, variableName } from "#/modules/branches/branch-review";
import { requestPrCheck } from "./pr-check-request.server";
import { prDestinations } from "./pr-environment.repository.server";
import type { SaveConditionalSave, WithdrawConditionalSave } from "./conditional-save";
import { conditionalSave, prEnvironment, type HeldRow } from "./tables";

/** The PR Environment, checked against the actor's organization. */
const prEnvironmentFor = Effect.fn("PrEnvironments.prEnvironmentFor")(function* (actor: Actor, input: { organizationSlug: string; prEnvironmentId: string }) {
  const context = yield* getEnvironmentContextForActorById(actor, { organizationSlug: input.organizationSlug, environmentId: input.prEnvironmentId });
  if (context === null) return yield* new NotFound({ message: "The PR environment was not found." });
  return context;
});

/**
 * Save the kept rows of a PR Environment's changes for one Destination: the Conditional Save, which goes live with the
 * pull request's merge commit. The rows are recomputed from authoritative states exactly as the browser did; a different
 * review string is refused. It stores what landing needs, so landing never reads the PR Environment. Saving again
 * replaces it.
 */
export const saveConditionalSave = Effect.fn("PrEnvironments.saveConditionalSave")(function* (actor: Actor, input: SaveConditionalSave) {
  const { project, environment: prEnvironmentRow } = yield* prEnvironmentFor(actor, input);
  const encryption = yield* SecretEncryption;
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // The Project, the Branch row, then the PR Environment's document, whose revision the save records.
    const branch = yield* lockBranchScope(project.id, input.prEnvironmentId, "share");
    if (!branch) return yield* new NotFound({ message: "The PR environment was not found." });
    // Under its document, as the close settles its saves: one made after that would never land or drop.
    const document = yield* loadEnvironmentDocument(input.prEnvironmentId, true);
    const [pullRequest] = yield* drizzle.select().from(prEnvironment).where(eq(prEnvironment.environmentId, input.prEnvironmentId));
    if (!pullRequest) return yield* new NotFound({ message: "The PR environment was not found." });
    if (pullRequest.closed) return yield* new Conflict({ message: `PR #${pullRequest.number} is closed.` });
    if (pullRequest.retired) return yield* new Conflict({ message: "This PR environment is being replaced." });
    if (!(yield* prDestinations(input.prEnvironmentId)).includes(input.destinationEnvironmentId)) {
      return yield* new Conflict({ message: `Nothing deploys ${pullRequest.targetBranch} now. Review again.` });
    }

    const { from, into, compare } = yield* goesToComparison({
      projectSlug: project.slug, prEnvironment: prEnvironmentRow, branch, destinationEnvironmentId: input.destinationEnvironmentId,
    });
    const { rows, review } = yield* core("review", () => branchChanges(compare));
    if (review !== input.review) return yield* new Conflict({ message: "Changed since you reviewed. Review again." });
    const byKey = new Map(rows.flatMap((row) => row.role === "move" ? [[row.key, row] as const] : []));
    const unknown = input.picks.find((pick) => !byKey.has(pick.key));
    if (unknown) return yield* new Validation({ field: "picks", message: `${unknown.key} isn't one of the changes.` });
    if (input.picks.length === 0) return yield* new Validation({ field: "picks", message: "Keep a change to save." });
    const empty = input.picks.find((pick) => pick.option === "new" && pick.value === "");
    if (empty) return yield* new Validation({ field: "picks", message: `Enter a new value for ${variableName(empty)}.` });

    const picks = corePicks({ encryption, rows, into, picks: input.picks });
    // Core refuses picks it couldn't land, such as a new service's variable without the service.
    yield* core("picks", () => branchChanges({ ...compare, picks }));
    const held = input.picks.flatMap((pick): HeldRow[] => {
      const row = byKey.get(pick.key);
      return row ? [{ row: withoutSealedCiphertext(row), option: pick.option }] : [];
    });
    const values = {
      organizationId: project.organizationId, projectId: project.id,
      prEnvironmentId: input.prEnvironmentId, repositoryId: pullRequest.repositoryId, prNumber: pullRequest.number,
      destinationEnvironmentId: input.destinationEnvironmentId,
      rows: held, picks,
      landing: {
        base: branch.base, from, parent: compare.parent ?? null, hostnames: compare.hostnames,
        identities: yield* loadIdentitySources(input.prEnvironmentId, new Set(picks.map(rowLineage))),
      },
      workingRevision: document.revision, targetBranch: pullRequest.targetBranch,
      savedByUserId: actor.userId, savedAt: new Date(),
    };
    const [saved] = yield* drizzle.insert(conditionalSave).values(values)
      .onConflictDoUpdate({ target: [conditionalSave.prEnvironmentId, conditionalSave.destinationEnvironmentId], set: values })
      .returning({ id: conditionalSave.id });
    if (!saved) return yield* Effect.die("PostgreSQL did not return the Conditional Save.");
    yield* requestPrCheck(input.prEnvironmentId);
    return saved;
  }));
});

/** Undo: the save is withdrawn. */
export const withdrawConditionalSave = Effect.fn("PrEnvironments.withdrawConditionalSave")(function* (actor: Actor, input: WithdrawConditionalSave) {
  yield* prEnvironmentFor(actor, input);
  const { drizzle } = yield* Database;
  yield* drizzle.delete(conditionalSave).where(heldOn(input));
  yield* requestPrCheck(input.prEnvironmentId);
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
  const compare = saveInput({
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
