import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges, type BranchPick } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { environment, environmentBranch } from "#/modules/project/tables";
import { getEnvironmentContextForActorById } from "#/modules/environment-design/authoring-repository.server";
import { loadCurrentEnvironmentState, loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { captureEnvironmentNodeIntroduction } from "#/modules/environment-design/environment-node-introduction.repository.server";
import { parseDashboardEnvironmentIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadAppliedIntent } from "#/modules/environment-design/saved-state-operations.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { branchHostnameSuffix } from "./branch-plan";
import { rowLineage, usedLive } from "./branch-review";
import { assertBranchSettled } from "./branch-guard.server";
import { copyIdentityRows } from "./branch-operations.server";
import { liveOwner } from "./live-owner";
import type { UpdateBranch } from "./branch-schemas";

/**
 * Update: stages every change the Parent deployed since the base in the Branch's Working State and advances the base by
 * exactly what it took, in one transaction. With `only`, it turns that Live Node into an Own Copy instead, from the
 * Environment that runs it; a Volume it mounts comes along empty. Refused unless the Branch runs its Working State.
 */
export const updateBranch = Effect.fn("Branches.updateBranch")(function* (actor: Actor, input: UpdateBranch) {
  const context = yield* getEnvironmentContextForActorById(actor, input);
  if (context === null) return yield* new NotFound({ message: "The environment was not found." });
  const { project } = context;
  return yield* withMutationResult(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* lockEnvironmentDeploymentQueue(input.environmentId);
    const document = yield* loadEnvironmentDocument(input.environmentId, true);
    yield* requireDocumentRevision(document, input.revision);
    const [row] = yield* drizzle.select().from(environmentBranch).where(eq(environmentBranch.environmentId, document.id));
    if (!row) return yield* new Validation({ field: "environmentId", message: "Only a branch updates from its parent." });
    yield* assertBranchSettled(drizzle, document.id);

    const branches = yield* drizzle.select().from(environmentBranch).where(eq(environmentBranch.projectId, row.projectId));
    const sourceId = input.only ? yield* ownerOf(row.parentEnvironmentId, input.only, branches) : row.parentEnvironmentId;
    const [source] = yield* drizzle.select().from(environment).where(eq(environment.id, sourceId));
    if (!source) return yield* Effect.die("The source environment is missing.");
    const from = yield* loadAppliedIntent(source.id, source.namespace,
      yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: source.id }));
    const { intent: into } = yield* loadCurrentEnvironmentState(document.id);

    // An Own Copy is an introduction: not provided live, and neither it nor the Volumes it mounts in the base.
    if (input.only && into.services.some((node) => node.lineageId === input.only)) {
      return yield* new Conflict({ message: "This branch already has its own copy." });
    }
    const copied = input.only ? ownCopyLineages(from, into, input.only) : null;
    const base = copied ? {
      ...row.base,
      services: row.base.services.filter((node) => !copied.has(node.lineageId)),
      volumes: row.base.volumes.filter((node) => !copied.has(node.resourceLineageId)),
    } : row.base;
    const changes = (picks?: BranchPick[]) => Effect.try({
      try: () => branchChanges({
        base, from, into,
        provided: usedLive(into).filter((lineage) => !copied?.has(lineage)),
        hostnames: {
          from: branches.some((branch) => branch.environmentId === source.id) ? branchHostnameSuffix(project.slug, source.namespace) : "",
          into: branchHostnameSuffix(project.slug, document.namespace),
        },
        fromKept: false,
        picks,
      }),
      catch: (error) => new Conflict({ message: error instanceof Error ? error.message : String(error) }),
    });
    // Every move row, secrets with the source's value as when the Branch was made.
    const picks = (yield* changes()).rows
      .filter((change) => change.role === "move" && (!copied || copied.has(rowLineage(change))))
      .map((change): BranchPick => change.role === "move" && change.choice ? { key: change.key, choice: { option: "from" } } : { key: change.key });
    if (picks.length === 0) return yield* new Conflict({ message: `Nothing new in ${source.name}.` });
    const applied = yield* changes(picks);
    if (!applied.base) return yield* Effect.die("Core returned no base for an Update.");
    const next = parseDashboardEnvironmentIntent(applied.next);

    const services = next.services.filter((node) => !into.services.some((own) => own.id === node.id));
    const volumes = next.volumes.filter((node) => !into.volumes.some((own) => own.resourceId === node.resourceId));
    yield* copyIdentityRows({ project, sourceId: source.id, environmentId: document.id, services, volumes });
    const written = yield* writeEnvironmentDocument(document, next);
    for (const node of services) yield* captureEnvironmentNodeIntroduction({ environmentId: document.id, nodeType: "service", nodeId: node.id });
    for (const node of volumes) yield* captureEnvironmentNodeIntroduction({ environmentId: document.id, nodeType: "volume", nodeId: node.resourceId });
    yield* drizzle.update(environmentBranch).set({ base: parseDashboardEnvironmentIntent(applied.base) })
      .where(eq(environmentBranch.environmentId, document.id));
    return written;
  }));
});

/** The Environment that runs a Live Node: the Parent, or its nearest ancestor that does. */
const ownerOf = Effect.fn("Branches.ownerOf")(function* (
  parentId: string, lineageId: string, branches: Array<{ environmentId: string; parentEnvironmentId: string }>,
) {
  const parentOf = new Map(branches.map((branch) => [branch.environmentId, branch.parentEnvironmentId]));
  const applied = new Map<string, Set<string>>();
  for (let at: string | undefined = parentId; at && !applied.has(at); at = parentOf.get(at)) {
    const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: at });
    applied.set(at, new Set(projection.explicitStates.flatMap((state) => state.applied.nodes.map((node) => node.nodeLineageId))));
  }
  const owner = liveOwner(parentId, lineageId, branches, applied);
  if (!owner) return yield* new Conflict({ message: "No environment this branch comes from runs it." });
  return owner;
});

/** A Live Node's lineage and the Volumes it mounts where it runs that the Branch doesn't have yet. */
function ownCopyLineages(from: SavedEnvironmentIntent, into: SavedEnvironmentIntent, lineageId: string) {
  const mounts = from.services.find((node) => node.lineageId === lineageId)?.volumeAttachments ?? [];
  const volumes = from.volumes.filter((volume) => mounts.some((mount) => mount.volumeResourceId === volume.resourceId)
    && !into.volumes.some((own) => own.resourceLineageId === volume.resourceLineageId));
  return new Set([lineageId, ...volumes.map((volume) => volume.resourceLineageId)]);
}
