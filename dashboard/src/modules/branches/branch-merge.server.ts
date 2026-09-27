import "@tanstack/react-start/server-only";
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
import { assertBranchSettled } from "./branch-guard.server";
import { tryCloseBranch } from "./branch-close.server";
import { core, landChanges, loadIdentitySources } from "./branch-operations.server";
import { branchHostnameSuffix } from "./branch-plan";
import { mergeInput, rowLineage, variableName } from "./branch-review";
import type { MergeBranch, MergePick } from "./branch-schemas";

/**
 * Stage the picked rows of a Branch's review in its Destination's Working State. Nothing is published or deployed
 * there, and nothing there is deleted. After commit the Branch closes, unless it is kept or `thenClose` is off.
 */
export const mergeBranch = Effect.fn("Branches.mergeBranch")(function* (actor: Actor, input: MergeBranch) {
  const context = yield* getEnvironmentContextForActorById(actor, {
    organizationSlug: input.organizationSlug, environmentId: input.branchEnvironmentId,
  });
  if (context === null) return yield* new NotFound({ message: "The branch was not found." });
  const { project } = context;
  const encryption = yield* SecretEncryption;
  const merged = yield* withMutationResult(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // The Project, then the Branch row, before any queue (lock order: lockProjectDefault); its base advances below.
    const branch = yield* lockBranchScope(project.id, input.branchEnvironmentId, "update");
    if (!branch) return yield* new NotFound({ message: "The branch was not found." });
    if (branch.prNumber !== null) return yield* new Conflict({ message: `A PR environment lands when #${branch.prNumber} merges.` });
    const destinationId = branch.parentEnvironmentId;

    // 1–3. Lock the Destination, check its revision, and refuse a Branch that runs something other than its Working State.
    // Both queues, by id, before the Destination's document (lock order: lockProjectDefault).
    yield* lockEnvironmentDeploymentQueues([destinationId, input.branchEnvironmentId]);
    const document = yield* loadEnvironmentDocument(destinationId, true);
    yield* requireDocumentRevision(document, input.destinationRevision);
    yield* assertBranchSettled(drizzle, input.branchEnvironmentId);

    // Recompute the rows from authoritative states, sealed values included, exactly as the browser did.
    const { intent: from } = yield* loadCurrentEnvironmentState(input.branchEnvironmentId);
    const { intent: into } = yield* loadCurrentEnvironmentState(destinationId);
    const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: destinationId });
    const deployed = projection.explicitStates.find((state) => state.environmentId === destinationId)?.applied.nodes.length ?? 0;
    const [destinationBranch] = yield* drizzle.select({ id: environmentBranch.environmentId }).from(environmentBranch)
      .where(eq(environmentBranch.environmentId, destinationId));
    const compare = mergeInput({
      base: branch.base, kept: branch.kept, branch: from, parent: into,
      parentApplied: deployed ? yield* loadAppliedIntent(destinationId, document.namespace, projection) : null,
      hostnames: {
        branch: branchHostnameSuffix(project.slug, context.environment.namespace, true),
        parent: branchHostnameSuffix(project.slug, document.namespace, destinationBranch !== undefined),
      },
    });
    const rows = branchChanges(compare);
    if (rows.review !== input.review) return yield* new Conflict({ message: "Changes moved after this review. Review them again." });

    // 4. Apply the picks with core; new values are sealed here, never in the browser.
    // A new value, secret or not, lands; an empty one would leave the change behind in a Branch that may close.
    const empty = input.picks.find((pick) => pick.option === "new" && pick.value === "");
    if (empty) return yield* new Validation({ field: "picks", message: `Enter a new value for ${variableName(empty)}.` });
    const picks = corePicks({ encryption, rows: rows.rows, into, picks: input.picks });
    const changes = yield* core("picks", () => branchChanges({ ...compare, picks }));
    const next = parseDashboardEnvironmentIntent(changes.next);
    const kept = (ids: string[], nextIds: string[]) => ids.every((id) => nextIds.includes(id));
    if (!kept(into.services.map((node) => node.id), next.services.map((node) => node.id))
      || !kept(into.volumes.map((node) => node.resourceId), next.volumes.map((node) => node.resourceId))) {
      return yield* Effect.die("Merge would delete from the Destination.");
    }

    // 5. Identity rows for what arrives, then the Destination's Working State; the base advances by what landed.
    const written = yield* landChanges({
      project, sources: yield* loadIdentitySources(input.branchEnvironmentId), document, into, next, picks,
      advance: changes.base ? { branchEnvironmentId: input.branchEnvironmentId, base: changes.base } : undefined,
    });
    return { environment: written, kept: branch.kept };
  }));

  // 6. The Merge stands whether or not the close starts; the user is told the Branch is still open.
  const { environment, kept } = merged.data;
  const closed = kept || !input.thenClose ? false : yield* tryCloseBranch(input.branchEnvironmentId);
  return { data: { environment, closed } };
});

/**
 * The ticked rows as core's picks: new values sealed here, never in the browser (none for an empty one), and each
 * unticked variable of a ticked new service left out, which core would otherwise land with its default.
 */
export function corePicks({ encryption, rows, into, picks }: {
  encryption: Encryption; rows: BranchRow[]; into: SavedEnvironmentIntent; picks: readonly MergePick[];
}): BranchPick[] {
  const refs = environmentVariableReferences(into);
  const ticked = picks.map((pick): BranchPick => {
    if (pick.option !== "new") return pick.option ? { key: pick.key, choice: { option: pick.option } } : { key: pick.key };
    if (pick.value === "") return { key: pick.key, choice: { option: "new" } };
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
