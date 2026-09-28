import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges, type BranchPick, type BranchRow as ChangeRow } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { lockBranchScope } from "#/modules/environment-design/workspace-repository.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { environment, environmentBranch, type project } from "#/modules/project/tables";
import { getEnvironmentContextForActorById } from "#/modules/environment-design/authoring-repository.server";
import { loadCurrentEnvironmentState, loadEnvironmentDocument, requireDocumentRevision, type EnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { parseDashboardEnvironmentIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadAppliedIntent } from "#/modules/environment-design/saved-state-operations.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { branchHostnameSuffix } from "./branch-plan";
import { rowLineage, usedLive } from "./branch-review";
import { assertBranchSettled } from "./branch-guard.server";
import { loadAncestorApplied } from "./branch-admission.server";
import { core, landChanges, loadIdentitySources } from "./branch-operations.server";
import { liveOwner } from "./live-owner";
import type { MakeOwnCopy, UpdateBranch } from "./branch-schemas";

/**
 * Update: stages every change the Parent deployed since the base in the Branch's Working State and advances the base by
 * exactly what it took, in one transaction. Refused unless the Branch runs its Working State.
 */
export const updateBranch = Effect.fn("Branches.updateBranch")(function* (actor: Actor, input: UpdateBranch) {
  return yield* onSettledBranch(actor, input, (branch) => Effect.gen(function* () {
    const parent = yield* loadEnvironment(branch.row.parentEnvironmentId);
    return yield* stageFrom(branch, {
      source: parent, from: yield* appliedIntentOf(parent), base: branch.row.base, provided: usedLive(branch.into),
      take: () => true, nothing: `Nothing new in ${parent.name}.`,
    });
  }));
});

/**
 * Own Copy: turns a Live Node into an Own Copy, from the Environment that runs it; a Volume it mounts comes along empty.
 * Under the same gate as Update.
 */
export const makeOwnCopy = Effect.fn("Branches.makeOwnCopy")(function* (actor: Actor, input: MakeOwnCopy) {
  return yield* onSettledBranch(actor, input, (branch) => Effect.gen(function* () {
    if (branch.into.services.some((node) => node.lineageId === input.lineageId)) {
      return yield* new Conflict({ message: "This branch already has its own copy." });
    }
    const { owner, from } = yield* ownerOf(branch.document.id, input.lineageId);
    // An Own Copy is an introduction: not provided live, and neither it nor the Volumes it mounts in the base.
    const copied = ownCopyLineages(from, branch.into, input.lineageId);
    return yield* stageFrom(branch, {
      source: owner, from,
      base: {
        ...branch.row.base,
        services: branch.row.base.services.filter((node) => !copied.has(node.lineageId)),
        volumes: branch.row.base.volumes.filter((node) => !copied.has(node.resourceLineageId)),
      },
      provided: usedLive(branch.into).filter((lineage) => !copied.has(lineage)),
      take: (row) => copied.has(rowLineage(row)),
      nothing: `Nothing to copy from ${owner.name}.`,
    });
  }));
});

type SettledBranch = {
  project: typeof project.$inferSelect;
  document: EnvironmentDocument;
  row: typeof environmentBranch.$inferSelect;
  into: SavedEnvironmentIntent;
};

/** Locks a Branch's queue, checks its revision, refuses it unless it runs its Working State, then stages with `run`. */
const onSettledBranch = <E, R>(
  actor: Actor,
  input: { organizationSlug: string; environmentId: string; revision: string },
  run: (branch: SettledBranch) => Effect.Effect<EnvironmentDocument, E, R>,
) => Effect.gen(function* () {
  const context = yield* getEnvironmentContextForActorById(actor, input);
  if (context === null) return yield* new NotFound({ message: "The environment was not found." });
  return yield* withMutationResult(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // The Project, then the Branch row, before its queue (lock order: lockProjectDefault); its base advances when the
    // changes land.
    const row = yield* lockBranchScope(context.project.id, input.environmentId, "update");
    yield* lockEnvironmentDeploymentQueue(input.environmentId);
    const document = yield* loadEnvironmentDocument(input.environmentId, true);
    yield* requireDocumentRevision(document, input.revision);
    if (!row) return yield* new Validation({ field: "environmentId", message: "Only a branch updates from its parent." });
    yield* assertBranchSettled(drizzle, document.id);
    const { intent: into } = yield* loadCurrentEnvironmentState(document.id);
    return yield* run({ project: context.project, document, row, into });
  }));
});

/** Stages the move rows `take` keeps, from `source`'s Applied State `from`, and advances the base by what landed. */
const stageFrom = Effect.fn("Branches.stageFrom")(function* ({ project, document, into }: SettledBranch, { source, from, base, provided, take, nothing }: {
  source: EnvironmentDocument; from: SavedEnvironmentIntent; base: SavedEnvironmentIntent; provided: string[];
  take: (row: ChangeRow) => boolean; nothing: string;
}) {
  const { drizzle } = yield* Database;
  const [sourceBranch] = yield* drizzle.select({ id: environmentBranch.environmentId }).from(environmentBranch)
    .where(eq(environmentBranch.environmentId, source.id));
  const changes = (picks?: BranchPick[]) => core("picks", () => branchChanges({
    base, from, into, provided,
    hostnames: {
      from: branchHostnameSuffix(project.slug, source.namespace, sourceBranch !== undefined),
      into: branchHostnameSuffix(project.slug, document.namespace, true),
    },
    fromKept: false,
    picks,
  }));
  // Every move row, secrets with the source's value as when the Branch was made.
  const picks = (yield* changes()).rows
    .filter((row) => row.role === "move" && take(row))
    .map((row): BranchPick => row.role === "move" && row.choice ? { key: row.key, choice: { option: "from" } } : { key: row.key });
  if (picks.length === 0) return yield* new Conflict({ message: nothing });
  const applied = yield* changes(picks);
  if (!applied.base) return yield* Effect.die("Core returned no base for an Update.");
  return yield* landChanges({
    project, sources: yield* loadIdentitySources(source.id), document, into, next: parseDashboardEnvironmentIntent(applied.next), picks,
    advance: { branchEnvironmentId: document.id, base: applied.base },
  });
});

const loadEnvironment = Effect.fn("Branches.loadEnvironment")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(environment).where(eq(environment.id, environmentId));
  if (!row) return yield* Effect.die("An environment this branch comes from is missing.");
  return row;
});

const appliedIntentOf = (source: EnvironmentDocument) => Effect.gen(function* () {
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: source.id });
  return yield* loadAppliedIntent(source.id, source.namespace, projection);
});

/** The Environment that runs a Live Node (the Parent, or its nearest ancestor that does), and its Applied State. */
const ownerOf = Effect.fn("Branches.ownerOf")(function* (environmentId: string, lineageId: string) {
  const { parentId, branches, runs, projection } = yield* loadAncestorApplied(environmentId);
  const ownerId = parentId ? liveOwner(parentId, lineageId, branches, runs) : null;
  if (!ownerId || !projection) return yield* new Conflict({ message: "No environment this branch comes from runs it." });
  const owner = yield* loadEnvironment(ownerId);
  return { owner, from: yield* loadAppliedIntent(owner.id, owner.namespace, projection) };
});

/** A Live Node's lineage and the Volumes it mounts where it runs that the Branch doesn't have yet. */
function ownCopyLineages(from: SavedEnvironmentIntent, into: SavedEnvironmentIntent, lineageId: string) {
  const mounts = from.services.find((node) => node.lineageId === lineageId)?.volumeAttachments ?? [];
  const volumes = from.volumes.filter((volume) => mounts.some((mount) => mount.volumeResourceId === volume.resourceId)
    && !into.volumes.some((own) => own.resourceLineageId === volume.resourceLineageId));
  return new Set([lineageId, ...volumes.map((volume) => volume.resourceLineageId)]);
}
