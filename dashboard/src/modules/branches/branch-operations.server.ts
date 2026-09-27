import "@tanstack/react-start/server-only";
import { eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { Database, isUniqueViolation } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { environment, environmentBranch, type project } from "#/modules/project/tables";
import { environmentCanvasNodePosition, environmentResource, service, serviceRegistryCredential } from "#/modules/environment-design/tables";
import { getEnvironmentContextForActorById } from "#/modules/environment-design/authoring-repository.server";
import { createEnvironmentRecord } from "#/modules/environment-design/workspace-repository.server";
import { loadCurrentEnvironmentSnapshotProjection, loadCurrentEnvironmentState, writeEnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { captureEnvironmentNodeIntroduction } from "#/modules/environment-design/environment-node-introduction.repository.server";
import { emptyEnvironmentIntent, parseDashboardEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "#/modules/environment-design/working-state-fingerprint.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { createManualEnvironmentDeployment } from "#/modules/deployments/deployment-command.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/runtime-lifecycle.repository.server";
import { branchHostnameSuffix, branchNameError, branchNamespace, liveLineages, ownLineages, planBranchOf } from "./branch-plan";
import type { CreateBranch } from "./branch-schemas";

type EnvironmentRow = typeof environment.$inferSelect;
type ProjectRow = typeof project.$inferSelect;

/** A core refusal (a ConfigError thrown through WebAssembly) as a field error. */
const core = <A>(field: string, run: () => A) => Effect.try({
  try: run,
  catch: (error) => new Validation({ field, message: error instanceof Error ? error.message : String(error) }),
});

/**
 * Make a Branch of the Parent and admit its first deployment, in one transaction; then dispatch it.
 * Core re-plans the picks, derives the Branch's configuration and returns its base.
 */
export const createBranch = Effect.fn("Branches.createBranch")(function* (actor: Actor, input: CreateBranch) {
  const context = yield* getEnvironmentContextForActorById(actor, {
    organizationSlug: input.organizationSlug, environmentId: input.parentEnvironmentId,
  });
  if (context === null) return yield* new NotFound({ message: "The environment was not found." });
  const created = yield* withMutationResult(Effect.gen(function* () {
    const branch = yield* writeBranch({ actor, project: context.project, parent: context.environment, input });
    const projection = yield* loadCurrentEnvironmentSnapshotProjection(branch.environment.id);
    const deployment = yield* createManualEnvironmentDeployment({
      environmentId: branch.environment.id,
      actorId: actor.userId,
      message: null,
      review: {
        savedStateBasis: { kind: "no_saved_state" },
        workingStateFingerprint: fingerprintReviewedEnvironmentWorkingStateSync(projection),
        destructiveServiceIds: [],
        destructiveVolumeReviews: [],
      },
    });
    return { ...branch, deploymentId: deployment.environmentDeploymentId };
  }), { isolationLevel: "read committed" }).pipe(
    Effect.catchIf(isUniqueViolation, () => new Conflict({ message: `${input.name} is taken in this organization.` })),
  );
  // A failed dispatch leaves the attempt queued; the Deployment Page can dispatch it again.
  yield* dispatchEnvironmentDeployment({
    environmentDeploymentId: created.data.deploymentId, environmentId: created.data.environment.id,
  }).pipe(Effect.catchTag("InngestEventSendError", () => Effect.void));
  return created;
});

/** Steps 1–5 of creation: configuration, Environment row, identity rows, Working State, Branch row. */
const writeBranch = Effect.fn("Branches.writeBranch")(function* ({ actor, project, parent, input }: {
  actor: Actor; project: ProjectRow; parent: EnvironmentRow; input: CreateBranch;
}) {
  const { drizzle } = yield* Database;
  const { intent: from } = yield* loadCurrentEnvironmentState(parent.id);
  const applied = (yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: parent.id }))
    .explicitStates.find((state) => state.environmentId === parent.id)?.applied.nodes ?? [];
  const plan = yield* core("picks", () => planBranchOf({
    parent: from, deployed: applied.map((node) => node.nodeLineageId), focus: input.focus, picks: input.picks,
  }));
  const own = ownLineages(plan);
  if (own.length === 0) return yield* new Validation({ field: "picks", message: "Pick something to copy." });

  // The browser checks these first; the unique index settles a race.
  const namespace = branchNamespace(project.slug, input.name);
  const nameError = branchNameError(project.slug, input.name, new Set());
  if (nameError) return yield* new Validation({ field: "name", message: nameError });
  const [parentBranch] = yield* drizzle.select().from(environmentBranch).where(eq(environmentBranch.environmentId, parent.id));

  // 1. Core derives the Branch's configuration: fresh ids, the Parent's lineages, secrets with their values.
  const changes = yield* core("picks", () => branchChanges({
    base: null,
    // SAFETY: core parses the Dashboard's document, variables included, in the same shape.
    from: from as never,
    into: emptyEnvironmentIntent(namespace) as never,
    provided: liveLineages(plan),
    hostnames: { from: parentBranch ? branchHostnameSuffix(project.slug, parent.namespace) : "", into: branchHostnameSuffix(project.slug, namespace) },
    fromKept: false,
    picks: own.map((lineage) => ({ key: `${lineage}:node` })),
  }));
  const next = parseDashboardEnvironmentIntent(changes.next);

  // 2. The Environment row; a taken namespace fails the unique index.
  const document = yield* createEnvironmentRecord({
    projectId: project.id, organizationId: project.organizationId, name: input.name.trim(), namespace,
  });

  // 3. Identity rows under core's fresh ids: lineage reused; names, policy, credentials and positions copied.
  const services = yield* drizzle.select().from(service).where(eq(service.environmentId, parent.id));
  const credentials = yield* drizzle.select().from(serviceRegistryCredential)
    .where(inArray(serviceRegistryCredential.serviceId, services.map((row) => row.id)));
  const resources = yield* drizzle.select().from(environmentResource).where(eq(environmentResource.environmentId, parent.id));
  const positions = yield* drizzle.select().from(environmentCanvasNodePosition).where(eq(environmentCanvasNodePosition.environmentId, parent.id));
  const copies: Array<{ from: string; to: string }> = [];
  for (const node of next.services) {
    const source = services.find((row) => row.lineageId === node.lineageId);
    if (!source) return yield* Effect.die(`Parent service for lineage ${node.lineageId} is missing.`);
    yield* drizzle.insert(service).values({
      id: node.id, organizationId: project.organizationId, projectId: project.id, environmentId: document.id,
      lineageId: node.lineageId, name: source.name, policy: source.policy, hasRegistryCredential: source.hasRegistryCredential,
    });
    const credential = credentials.find((row) => row.serviceId === source.id);
    if (credential) yield* drizzle.insert(serviceRegistryCredential).values({
      organizationId: project.organizationId, serviceId: node.id,
      encryptedRegistryUsername: credential.encryptedRegistryUsername, encryptedRegistrySecret: credential.encryptedRegistrySecret,
    });
    copies.push({ from: source.id, to: node.id });
  }
  for (const node of next.volumes) {
    const source = resources.find((row) => row.lineageId === node.resourceLineageId);
    if (!source) return yield* Effect.die(`Parent volume for lineage ${node.resourceLineageId} is missing.`);
    yield* drizzle.insert(environmentResource).values({
      id: node.resourceId, organizationId: project.organizationId, projectId: project.id, environmentId: document.id,
      lineageId: node.resourceLineageId, implementationType: "volume",
    });
    copies.push({ from: source.id, to: node.resourceId });
  }
  const copiedPositions = copies.flatMap(({ from: sourceId, to }) => positions
    .filter((row) => row.resourceId === sourceId)
    .map((row) => ({ organizationId: project.organizationId, environmentId: document.id, resourceType: row.resourceType, resourceId: to, x: row.x, y: row.y })));
  if (copiedPositions.length) yield* drizzle.insert(environmentCanvasNodePosition).values(copiedPositions);

  // 4. Working State, then each node's Node Introduction.
  const written = yield* writeEnvironmentDocument(document, next);
  for (const node of next.services) yield* captureEnvironmentNodeIntroduction({ environmentId: document.id, nodeType: "service", nodeId: node.id });
  for (const node of next.volumes) yield* captureEnvironmentNodeIntroduction({ environmentId: document.id, nodeType: "volume", nodeId: node.resourceId });

  // 5. The Branch row, with the base core returned.
  if (!changes.base) return yield* Effect.die("Core returned no base for a new Branch.");
  const [branch] = yield* drizzle.insert(environmentBranch).values({
    environmentId: document.id, organizationId: project.organizationId, projectId: project.id,
    parentEnvironmentId: parent.id, kept: input.keep,
    base: parseDashboardEnvironmentIntent(changes.base),
    createdByUserId: actor.userId,
  }).returning();
  if (!branch) return yield* Effect.die("PostgreSQL did not return the Branch row.");
  return { environment: written, branch };
});
