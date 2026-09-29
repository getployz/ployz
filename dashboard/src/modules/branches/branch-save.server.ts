import "@tanstack/react-start/server-only";
import { prEnvironment } from "#/modules/pr-environments/tables";
import { randomUUID } from "node:crypto";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges, type BranchPick, type BranchRow } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { lockBranchScope } from "#/modules/environment-design/workspace-repository.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { environmentBranch } from "#/modules/project/tables";
import { getEnvironmentContextForActorById } from "#/modules/environment-design/authoring-repository.server";
import { loadCurrentEnvironmentState, loadEnvironmentDocument, requireDocumentRevision } from "#/modules/environment-design/working-state-repository.server";
import { parseDashboardEnvironmentIntent, savedVariableIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { environmentVariableReferences } from "#/modules/environment-design/variable-document";
import { variableValueColumnsForWrite } from "#/modules/environment-design/variable-repository.server";
import { loadAppliedIntent } from "#/modules/environment-design/saved-state-operations.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { lockEnvironmentDeploymentQueues } from "#/modules/deployments/queue-lock.server";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { tryCloseBranch } from "./branch-close.server";
import { core, landChanges, loadIdentitySources } from "./branch-operations.server";
import { branchHostnameSuffix } from "./branch-plan";
import { saveInput, rowLineage, variableName } from "./branch-review";
import type { SaveBranch, SavePick } from "./branch-schemas";

/**
 * Save: stage the picked rows of a Branch's review in its Destination's Working State. Nothing is published or deployed
 * there, and nothing there is deleted. It never waits for the Branch to deploy: the rows are its Working State, staged or
 * not, running or not. After commit the Branch is deleted, unless it is kept or `thenDelete` is off.
 */
export const saveBranch = Effect.fn("Branches.saveBranch")(function* (actor: Actor, input: SaveBranch) {
  const context = yield* getEnvironmentContextForActorById(actor, {
    organizationSlug: input.organizationSlug, environmentId: input.branchEnvironmentId,
  });
  if (context === null) return yield* new NotFound({ message: "The branch was not found." });
  const { project } = context;
  const encryption = yield* SecretEncryption;
  const saved = yield* withMutationResult(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // The Project, then the Branch row, before any queue (lock order: lockProjectDefault); its base advances below.
    const branch = yield* lockBranchScope(project.id, input.branchEnvironmentId, "update");
    if (!branch) return yield* new NotFound({ message: "The branch was not found." });
    const [pullRequest] = yield* drizzle.select({ number: prEnvironment.number }).from(prEnvironment).where(eq(prEnvironment.environmentId, branch.environmentId));
    if (pullRequest) return yield* new Conflict({ message: `A PR environment's changes go live with PR #${pullRequest.number}.` });
    const destinationId = branch.parentEnvironmentId;

    // 1–2. Lock the Destination and check its revision. Both queues, by id, before the Destination's document (lock
    // order: lockProjectDefault).
    yield* lockEnvironmentDeploymentQueues([destinationId, input.branchEnvironmentId]);
    // A close admits under the same locks; a review left open after it would stage changes the Branch no longer offers.
    if ((yield* activeTeardownFor([input.branchEnvironmentId])).size > 0) {
      return yield* new Conflict({ message: "This branch is closing.", userFacing: true });
    }
    const document = yield* loadEnvironmentDocument(destinationId, true);
    yield* requireDocumentRevision(document, input.destinationRevision);

    // Recompute the rows from authoritative states, sealed values included, exactly as the browser did.
    const { intent: from } = yield* loadCurrentEnvironmentState(input.branchEnvironmentId);
    const { intent: into } = yield* loadCurrentEnvironmentState(destinationId);
    const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: destinationId });
    const deployed = projection.explicitStates.find((state) => state.environmentId === destinationId)?.applied.nodes.length ?? 0;
    const [destinationBranch] = yield* drizzle.select({ id: environmentBranch.environmentId }).from(environmentBranch)
      .where(eq(environmentBranch.environmentId, destinationId));
    const compare = saveInput({
      base: branch.base, kept: branch.kept, branch: from, parent: into,
      parentApplied: deployed ? yield* loadAppliedIntent(destinationId, document.namespace, projection) : null,
      hostnames: {
        branch: branchHostnameSuffix(project.slug, context.environment.namespace, true),
        parent: branchHostnameSuffix(project.slug, document.namespace, destinationBranch !== undefined),
      },
    });
    const rows = branchChanges(compare);
    if (rows.review !== input.review) return yield* new Conflict({ message: "Changed since you reviewed. Review again." });

    // 3. Apply the picks with core; new values are sealed here, never in the browser.
    // A new value, secret or not, lands; an empty one would leave the change behind in a Branch that may close.
    const empty = input.picks.find((pick) => pick.option === "new" && pick.value === "");
    if (empty) return yield* new Validation({ field: "picks", message: `Enter a new value for ${variableName(empty)}.` });
    const picks = corePicks({ encryption, rows: rows.rows, into, picks: input.picks });
    const changes = yield* core("picks", () => branchChanges({ ...compare, picks }));
    const next = parseDashboardEnvironmentIntent(changes.next);
    const kept = (ids: string[], nextIds: string[]) => ids.every((id) => nextIds.includes(id));
    if (!kept(into.services.map((node) => node.id), next.services.map((node) => node.id))
      || !kept(into.volumes.map((node) => node.resourceId), next.volumes.map((node) => node.resourceId))) {
      return yield* Effect.die("Save would delete from the Destination.");
    }

    // 4. Identity rows for what arrives, then the Destination's Working State; the base advances by what landed.
    const written = yield* landChanges({
      project, sources: yield* loadIdentitySources(input.branchEnvironmentId), document, into, next, picks,
      advance: changes.base ? { branchEnvironmentId: input.branchEnvironmentId, base: changes.base } : undefined,
    });
    return { environment: written, kept: branch.kept };
  }));

  // 5. The Save stands whether or not the delete starts; the user is told the Branch is still there.
  const { environment, kept } = saved.data;
  const closed = kept || !input.thenDelete ? false : yield* tryCloseBranch(input.branchEnvironmentId);
  return { data: { environment, closed } };
});

/**
 * The ticked rows as core's picks: new values sealed here, never in the browser, and each unticked variable of a ticked
 * new service left out, which core would otherwise land with its default. The caller refuses an empty new value.
 */
export function corePicks({ encryption, rows, into, picks }: {
  encryption: Encryption; rows: BranchRow[]; into: SavedEnvironmentIntent; picks: readonly SavePick[];
}): BranchPick[] {
  const refs = environmentVariableReferences(into);
  const ticked = picks.map((pick): BranchPick => {
    if (pick.option !== "new") return pick.option ? { key: pick.key, choice: { option: pick.option } } : { key: pick.key };
    return { key: pick.key, choice: { option: "new", value: sealValue(encryption, rows, into, pick.key, pick.value, refs) } };
  });
  const newServices = new Set(ticked.flatMap((pick) => pick.key.endsWith(":node") ? [rowLineage(pick)] : []));
  const leftOut = rows.flatMap((row): BranchPick[] => row.role === "move" && row.choice && newServices.has(rowLineage(row))
    && !ticked.some((pick) => pick.key === row.key) ? [{ key: row.key, choice: { option: "leave_out" } }] : []);
  return [...ticked, ...leftOut];
}

/** A new value for row `key`, sealed when the row is a secret. */
export function sealValue(
  encryption: Encryption, rows: BranchRow[], into: SavedEnvironmentIntent, key: string, value: string,
  refs = environmentVariableReferences(into),
) {
  const row = rows.find((candidate) => candidate.key === key);
  const secret = row?.role === "move" && row.choice?.secret === true;
  const variable = savedVariableIntent({
    id: randomUUID(), key, description: null, exported: false,
    ...variableValueColumnsForWrite(encryption, { type: secret ? "sealed" : "plain", value }, refs.lookupLineage),
  });
  return { value: variable.value, valueFingerprint: variable.valueFingerprint };
}

type Encryption = Parameters<typeof variableValueColumnsForWrite>[0];
