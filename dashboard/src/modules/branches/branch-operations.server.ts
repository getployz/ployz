import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
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
import { emptyEnvironmentIntent, parseDashboardEnvironmentIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "#/modules/environment-design/working-state-fingerprint.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { environmentDeployment } from "#/modules/deployments/tables";
import { loadEnvironmentSavedIntentById } from "#/modules/environment-design/saved-state-repository.server";
import { loadAppliedIntent } from "#/modules/environment-design/saved-state-operations.server";
import { createManualEnvironmentDeployment } from "#/modules/deployments/deployment-command.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/runtime-lifecycle.repository.server";
import { branchHostnameSuffix, branchNameError, branchNamespace, liveLineages, ownLineages, planBranchOf } from "./branch-plan";
import type { CreateBranch, SetBranchSetupDefaults } from "./branch-schemas";

type EnvironmentRow = typeof environment.$inferSelect;
type ProjectRow = typeof project.$inferSelect;

/** A core refusal (a ConfigError thrown through WebAssembly) as a field error. */
const core = <A>(field: string, run: () => A) => Effect.try({
  try: run,
  catch: (error) => new Validation({ field, message: error instanceof Error ? error.message : String(error) }),
});

/**
 * Make a Branch of the Parent and, with `deployNow`, admit its first deployment, in one transaction; then dispatch it.
 * Core re-plans the picks, derives the Branch's configuration and returns its base.
 */
export const createBranch = Effect.fn("Branches.createBranch")(function* (actor: Actor, input: CreateBranch) {
  const context = yield* getEnvironmentContextForActorById(actor, {
    organizationSlug: input.organizationSlug, environmentId: input.parentEnvironmentId,
  });
  if (context === null) return yield* new NotFound({ message: "The environment was not found." });
  const created = yield* withMutationResult(Effect.gen(function* () {
    const branch = yield* writeBranch({ actor, project: context.project, parent: context.environment, input });
    if (!input.deployNow) return { ...branch, deploymentId: null };
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
  const { deploymentId } = created.data;
  if (deploymentId === null) return created;
  // A dispatch failure fails the attempt; the Branch stays and deploys again from its canvas.
  yield* dispatchEnvironmentDeployment({
    environmentDeploymentId: deploymentId, environmentId: created.data.environment.id,
  }).pipe(Effect.catchTag("InngestEventSendError", () => Effect.void));
  return created;
});

/** Steps 1–5 of creation: configuration, Environment row, identity rows, Working State, Branch row. */
const writeBranch = Effect.fn("Branches.writeBranch")(function* ({ actor, project, parent, input }: {
  actor: Actor; project: ProjectRow; parent: EnvironmentRow; input: CreateBranch;
}) {
  const { drizzle } = yield* Database;
  const { intent: working } = yield* loadCurrentEnvironmentState(parent.id);
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: parent.id });
  const applied = projection.explicitStates.find((state) => state.environmentId === parent.id)?.applied.nodes ?? [];
  const plan = yield* core("picks", () => planBranchOf({
    parent: working, deployed: applied.map((node) => node.nodeLineageId), focus: input.focus, picks: input.picks,
  }));
  const own = ownLineages(plan);
  if (own.length === 0) return yield* new Validation({ field: "picks", message: "Pick something to copy." });
  const ownServices = new Set(working.services.map((node) => node.lineageId).filter((lineage) => own.includes(lineage)));
  if (input.setupCommands.some((setup) => !ownServices.has(setup.lineageId))) {
    return yield* new Validation({ field: "setupCommands", message: "A setup command runs in one of the branch's own services." });
  }

  // Fix it on a branch: the failed configuration of that service stands in for the Parent's; the Parent stays as it is.
  const failed = input.fix ? yield* loadFailedService(parent.id, input.fix) : null;
  if (failed && !(own.includes(failed.lineageId) && working.services.some((node) => node.lineageId === failed.lineageId))) {
    return yield* new Validation({ field: "fix", message: "The failed service must get its own copy." });
  }
  const from = failed
    ? { ...working, services: working.services.map((node) => node.lineageId === failed.lineageId ? failed : node) }
    : working;

  // The browser checks these first; the unique index settles a race.
  const namespace = branchNamespace(project.slug, input.name);
  const nameError = branchNameError(project.slug, input.name, new Set());
  if (nameError) return yield* new Validation({ field: "name", message: nameError });
  const [parentBranch] = yield* drizzle.select().from(environmentBranch).where(eq(environmentBranch.environmentId, parent.id));

  // 1. Core derives the Branch's configuration: fresh ids, the Parent's lineages, secrets with their values.
  const create = (source: typeof from, picks: string[]) => core("picks", () => branchChanges({
    base: null,
    from: source,
    into: emptyEnvironmentIntent(namespace),
    provided: liveLineages(plan),
    hostnames: { from: parentBranch ? branchHostnameSuffix(project.slug, parent.namespace) : "", into: branchHostnameSuffix(project.slug, namespace) },
    fromKept: false,
    picks: picks.map((lineage) => ({ key: `${lineage}:node` })),
  }));
  const changes = yield* create(from, own);
  const next = parseDashboardEnvironmentIntent(changes.next);
  // A fix's base is the Parent's Applied State (minus what it uses live), so the failed change shows as staged.
  const base = failed
    ? (yield* create(yield* loadAppliedIntent(parent.id, parent.namespace, projection), [])).base
    : changes.base;

  // 2. The Environment row; a taken namespace fails the unique index.
  const document = yield* createEnvironmentRecord({
    projectId: project.id, organizationId: project.organizationId, name: input.name.trim(), namespace,
  });

  // 3. Identity rows under core's fresh ids.
  yield* copyIdentityRows({ project, sourceId: parent.id, environmentId: document.id, services: next.services, volumes: next.volumes });

  // 4. Working State, then each node's Node Introduction.
  const written = yield* writeEnvironmentDocument(document, next);
  for (const node of next.services) yield* captureEnvironmentNodeIntroduction({ environmentId: document.id, nodeType: "service", nodeId: node.id });
  for (const node of next.volumes) yield* captureEnvironmentNodeIntroduction({ environmentId: document.id, nodeType: "volume", nodeId: node.resourceId });

  // 5. The Branch row, with the base core returned.
  if (!base) return yield* Effect.die("Core returned no base for a new Branch.");
  const [branch] = yield* drizzle.insert(environmentBranch).values({
    environmentId: document.id, organizationId: project.organizationId, projectId: project.id,
    parentEnvironmentId: parent.id, kept: input.keep,
    base: parseDashboardEnvironmentIntent(base),
    setupCommands: input.setupCommands,
    createdByUserId: actor.userId,
  }).returning();
  if (!branch) return yield* Effect.die("PostgreSQL did not return the Branch row.");
  return { environment: written, branch };
});

/** Identity rows for nodes core introduced under fresh ids: lineage reused; names, policy, credentials and positions copied from the source Environment. */
export const copyIdentityRows = Effect.fn("Branches.copyIdentityRows")(function* ({ project, sourceId, environmentId, services: nodes, volumes }: {
  project: ProjectRow; sourceId: string; environmentId: string;
  services: SavedEnvironmentIntent["services"]; volumes: SavedEnvironmentIntent["volumes"];
}) {
  const { drizzle } = yield* Database;
  const services = yield* drizzle.select().from(service).where(eq(service.environmentId, sourceId));
  const credentials = yield* drizzle.select().from(serviceRegistryCredential)
    .where(inArray(serviceRegistryCredential.serviceId, services.map((row) => row.id)));
  const resources = yield* drizzle.select().from(environmentResource).where(eq(environmentResource.environmentId, sourceId));
  const positions = yield* drizzle.select().from(environmentCanvasNodePosition).where(eq(environmentCanvasNodePosition.environmentId, sourceId));
  const copies: Array<{ from: string; to: string }> = [];
  for (const node of nodes) {
    const source = services.find((row) => row.lineageId === node.lineageId);
    if (!source) return yield* Effect.die(`Source service for lineage ${node.lineageId} is missing.`);
    yield* drizzle.insert(service).values({
      id: node.id, organizationId: project.organizationId, projectId: project.id, environmentId,
      lineageId: node.lineageId, name: source.name, policy: source.policy, hasRegistryCredential: source.hasRegistryCredential,
    });
    const credential = credentials.find((row) => row.serviceId === source.id);
    if (credential) yield* drizzle.insert(serviceRegistryCredential).values({
      organizationId: project.organizationId, serviceId: node.id,
      encryptedRegistryUsername: credential.encryptedRegistryUsername, encryptedRegistrySecret: credential.encryptedRegistrySecret,
    });
    copies.push({ from: source.id, to: node.id });
  }
  for (const node of volumes) {
    const source = resources.find((row) => row.lineageId === node.resourceLineageId);
    if (!source) return yield* Effect.die(`Source volume for lineage ${node.resourceLineageId} is missing.`);
    yield* drizzle.insert(environmentResource).values({
      id: node.resourceId, organizationId: project.organizationId, projectId: project.id, environmentId,
      lineageId: node.resourceLineageId, implementationType: "volume",
    });
    copies.push({ from: source.id, to: node.resourceId });
  }
  const copiedPositions = copies.flatMap(({ from: sourceNodeId, to }) => positions
    .filter((row) => row.resourceId === sourceNodeId)
    .map((row) => ({ organizationId: project.organizationId, environmentId, resourceType: row.resourceType, resourceId: to, x: row.x, y: row.y })));
  if (copiedPositions.length) yield* drizzle.insert(environmentCanvasNodePosition).values(copiedPositions);
});

/** The failed attempt's Saved configuration of one service. */
const loadFailedService = Effect.fn("Branches.loadFailedService")(function* (parentId: string, fix: NonNullable<CreateBranch["fix"]>) {
  const { drizzle } = yield* Database;
  const [attempt] = yield* drizzle.select({ status: environmentDeployment.status, savedStateSnapshotId: environmentDeployment.savedStateSnapshotId })
    .from(environmentDeployment)
    .where(and(eq(environmentDeployment.id, fix.deploymentId), eq(environmentDeployment.environmentId, parentId)));
  if (attempt?.status !== "failed") return yield* new Validation({ field: "fix", message: "Only a failed deployment can be fixed on a branch." });
  const saved = yield* loadEnvironmentSavedIntentById({ environmentId: parentId, savedStateSnapshotId: attempt.savedStateSnapshotId });
  const node = saved?.intent.services.find((candidate) => candidate.id === fix.serviceId);
  if (!node) return yield* new Validation({ field: "fix", message: "The failed service is not in that deployment." });
  return node;
});

/** Saves the Setup Commands that prefill new Branches of this Environment; saved at once, never staged. */
export const setBranchSetupDefaults = Effect.fn("Branches.setBranchSetupDefaults")(function* (actor: Actor, input: SetBranchSetupDefaults) {
  const context = yield* getEnvironmentContextForActorById(actor, {
    organizationSlug: input.organizationSlug, environmentId: input.environmentId,
  });
  if (context === null) return yield* new NotFound({ message: "The environment was not found." });
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.update(environment).set({ branchSetupCommands: input.setupCommands })
    .where(eq(environment.id, context.environment.id)).returning();
  if (!row) return yield* new NotFound({ message: "The environment was not found." });
  return row;
});
