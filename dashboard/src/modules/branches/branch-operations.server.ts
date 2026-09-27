import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges, planBranch, type BranchChanges } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { Database, isUniqueViolation } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { environment, environmentBranch, project as projectTable, type project } from "#/modules/project/tables";
import { environmentCanvasNodePosition, environmentResource, service, serviceRegistryCredential } from "#/modules/environment-design/tables";
import { getEnvironmentContextForActorById } from "#/modules/environment-design/authoring-repository.server";
import { createEnvironmentRecord, lockBranchScope } from "#/modules/environment-design/workspace-repository.server";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
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
import { branchHostnameSuffix, branchNameError, branchNamespace, liveLineages, ownLineages } from "./branch-plan";
import { rowLineage } from "./branch-review";
import type { CreateBranch, SetBranchSetupDefaults } from "./branch-schemas";
import { fromRepository, prEnvironmentIntent, pullRequestColumns, type PullRequestFacts } from "#/modules/pr-environments/pull-request";

type EnvironmentRow = typeof environment.$inferSelect;
type ProjectRow = typeof project.$inferSelect;

/** A core refusal (a ConfigError thrown through WebAssembly) as a field error. */
export const core = <A>(field: string, run: () => A) => Effect.try({
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
  return yield* writeAndDeploy({ actor, project: context.project, parent: context.environment, input });
});

/**
 * The system makes a pull request's PR Environment, `pr-<number>`, as a Branch of `parent` acting for `actor`, and admits
 * its first deployment. After derivation the repository's services track the pull request's head Git branch and deploy
 * on push, and every Own Copy runs one replica.
 */
export const createPrEnvironment = Effect.fn("Branches.createPrEnvironment")(function* ({ actor, parentEnvironmentId, focus, picks, setupCommands, pullRequest }: {
  actor: Actor; parentEnvironmentId: string; focus: string[];
  picks: CreateBranch["picks"]; setupCommands: CreateBranch["setupCommands"]; pullRequest: PullRequestFacts;
}) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ environment, project: projectTable }).from(environment)
    .innerJoin(projectTable, eq(projectTable.id, environment.projectId))
    .where(eq(environment.id, parentEnvironmentId));
  if (!row) return yield* new NotFound({ message: "The environment was not found." });
  return yield* writeAndDeploy({
    actor, project: row.project, parent: row.environment, pullRequest,
    input: { name: `pr-${pullRequest.number}`, focus, picks, keep: false, deployNow: true, setupCommands },
  });
});

type BranchInput = Omit<CreateBranch, "organizationSlug" | "parentEnvironmentId">;

/** Writes the Branch and, with `deployNow`, admits its first deployment, in one transaction; then dispatches it. */
const writeAndDeploy = Effect.fn("Branches.writeAndDeploy")(function* ({ actor, project, parent, input, pullRequest }: {
  actor: Actor; project: ProjectRow; parent: EnvironmentRow; input: BranchInput; pullRequest?: PullRequestFacts;
}) {
  const created = yield* withMutationResult(Effect.gen(function* () {
    const branch = yield* writeBranch({ actor, project, parent, input, pullRequest });
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
const writeBranch = Effect.fn("Branches.writeBranch")(function* ({ actor, project, parent, input, pullRequest }: {
  actor: Actor; project: ProjectRow; parent: EnvironmentRow; input: BranchInput; pullRequest?: PullRequestFacts;
}) {
  const { drizzle } = yield* Database;
  // Under the Project lock teardown admission takes: a Parent that is being torn down gets no new Branch, and one torn
  // down after this commits takes the new Branch with it. Then the Parent's Branch row, shared, so an idle close of the
  // Parent waits for this Branch and sees it (lock order: lockProjectDefault).
  const parentBranch = yield* lockBranchScope(project.id, parent.id, "share");
  if ((yield* activeTeardownFor([parent.id])).size > 0) {
    return yield* new Conflict({ message: `${parent.name} is being torn down.` });
  }
  const { intent: working } = yield* loadCurrentEnvironmentState(parent.id);
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: parent.id });
  const applied = projection.explicitStates.find((state) => state.environmentId === parent.id)?.applied.nodes ?? [];
  const plan = yield* core("picks", () => planBranch({
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

  // 1. Core derives the Branch's configuration: fresh ids, the Parent's lineages, secrets with their values.
  const create = (source: typeof from, picks: string[]) => core("picks", () => branchChanges({
    base: null,
    from: source,
    into: emptyEnvironmentIntent(namespace),
    provided: liveLineages(plan),
    hostnames: { from: branchHostnameSuffix(project.slug, parent.namespace, parentBranch !== null), into: branchHostnameSuffix(project.slug, namespace, true) },
    fromKept: false,
    picks: picks.map((lineage) => ({ key: `${lineage}:node` })),
  }));
  const changes = yield* create(from, own);
  const derived = parseDashboardEnvironmentIntent(changes.next);
  const next = pullRequest ? prEnvironmentIntent(derived, pullRequest) : derived;
  // A fix's base is the Parent's Applied State (minus what it uses live), so the failed change shows as staged.
  const base = failed
    ? (yield* create(yield* loadAppliedIntent(parent.id, parent.namespace, projection), [])).base
    : changes.base;

  // 2. The Environment row; a taken namespace fails the unique index.
  const document = yield* createEnvironmentRecord({
    projectId: project.id, organizationId: project.organizationId, name: input.name.trim(), namespace,
  });

  // 3–4. Identity rows under core's fresh ids, the Working State, then each node's Node Introduction.
  const written = yield* landChanges({ project, from: parent.id, document, into: emptyEnvironmentIntent(namespace), next, picks: [] });

  // 5. The Branch row, with the base core returned.
  if (!base) return yield* Effect.die("Core returned no base for a new Branch.");
  const [branch] = yield* drizzle.insert(environmentBranch).values({
    environmentId: document.id, organizationId: project.organizationId, projectId: project.id,
    parentEnvironmentId: parent.id, kept: input.keep,
    base: parseDashboardEnvironmentIntent(base),
    setupCommands: input.setupCommands,
    createdByUserId: actor.userId,
    ...(pullRequest ? pullRequestColumns(pullRequest) : {}),
  }).returning();
  if (!branch) return yield* Effect.die("PostgreSQL did not return the Branch row.");
  // The repository's services deploy on push whatever the Parent's Deployment Policy says; Wait for CI and watch paths stay.
  if (pullRequest) {
    for (const node of next.services) {
      if (!fromRepository(node.config, pullRequest.repositoryId)) continue;
      yield* drizzle.update(service).set({ policy: sql`${service.policy} || '{"autoDeploy":true}'::jsonb` }).where(eq(service.id, node.id));
    }
  }
  return { environment: written, branch };
});

/**
 * Lands core's `next` in `document`, whose Working State is `into`: identity rows for the nodes arriving from `from`, the
 * Working State, then each arrival's Node Introduction. A picked `source.credentials` row of a service `into` already has
 * brings its registry credential along; an arriving service brings its own. With `advance`, the Branch's base moves too.
 */
export const landChanges = Effect.fn("Branches.landChanges")(function* ({ project, from, document, into, next, picks, advance }: {
  project: ProjectRow; from: string; document: EnvironmentRow;
  into: SavedEnvironmentIntent; next: SavedEnvironmentIntent;
  picks: ReadonlyArray<{ key: string }>;
  advance?: { branchEnvironmentId: string; base: NonNullable<BranchChanges["base"]> };
}) {
  const { drizzle } = yield* Database;
  const services = next.services.filter((node) => !into.services.some((own) => own.id === node.id));
  const volumes = next.volumes.filter((node) => !into.volumes.some((own) => own.resourceId === node.resourceId));
  yield* copyIdentities({ project, from, to: document.id, services, volumes });
  const credentialLineages = picks.flatMap((pick) => pick.key.endsWith(":source.credentials") ? [rowLineage(pick)] : []);
  for (const lineageId of credentialLineages) {
    const receiver = into.services.find((node) => node.lineageId === lineageId);
    if (receiver) yield* copyCredential({ organizationId: project.organizationId, from, lineageId, to: receiver.id });
  }
  const written = yield* writeEnvironmentDocument(document, next);
  yield* captureIntroductions(document.id, services, volumes);
  if (advance) {
    yield* drizzle.update(environmentBranch).set({ base: parseDashboardEnvironmentIntent(advance.base) })
      .where(eq(environmentBranch.environmentId, advance.branchEnvironmentId));
  }
  return written;
});

/** The registry credential of `lineageId`'s service in `from`, as the credential of service `to`. */
const copyCredential = Effect.fn("Branches.copyCredential")(function* ({ organizationId, from, lineageId, to }: {
  organizationId: string; from: string; lineageId: string; to: string;
}) {
  const { drizzle } = yield* Database;
  const [source] = yield* drizzle.select({ username: serviceRegistryCredential.encryptedRegistryUsername, secret: serviceRegistryCredential.encryptedRegistrySecret })
    .from(serviceRegistryCredential).innerJoin(service, eq(service.id, serviceRegistryCredential.serviceId))
    .where(and(eq(service.environmentId, from), eq(service.lineageId, lineageId)));
  if (!source) return yield* Effect.die(`The registry credential for lineage ${lineageId} is missing.`);
  const credential = { organizationId, serviceId: to, revision: randomUUID(), encryptedRegistryUsername: source.username, encryptedRegistrySecret: source.secret };
  yield* drizzle.insert(serviceRegistryCredential).values(credential).onConflictDoUpdate({ target: serviceRegistryCredential.serviceId, set: credential });
  yield* drizzle.update(service).set({ hasRegistryCredential: true }).where(eq(service.id, to));
});

/**
 * Identity rows for nodes arriving in `to` under core's fresh ids, each from its lineage's node in `from`: lineage
 * reused; display name, Deployment Policy, registry credential and canvas position copied.
 */
const copyIdentities = Effect.fn("Branches.copyIdentities")(function* ({ project, from, to, services: arriving, volumes: arrivingVolumes }: {
  project: ProjectRow; from: string; to: string;
  services: ReadonlyArray<{ id: string; lineageId: string }>;
  volumes: ReadonlyArray<{ resourceId: string; resourceLineageId: string }>;
}) {
  const { drizzle } = yield* Database;
  const services = yield* drizzle.select().from(service).where(eq(service.environmentId, from));
  const resources = yield* drizzle.select().from(environmentResource).where(eq(environmentResource.environmentId, from));
  const positions = yield* drizzle.select().from(environmentCanvasNodePosition).where(eq(environmentCanvasNodePosition.environmentId, from));
  const copies: Array<{ from: string; to: string }> = [];
  for (const node of arriving) {
    const source = services.find((row) => row.lineageId === node.lineageId);
    if (!source) return yield* Effect.die(`Source service for lineage ${node.lineageId} is missing.`);
    yield* drizzle.insert(service).values({
      id: node.id, organizationId: project.organizationId, projectId: project.id, environmentId: to,
      lineageId: node.lineageId, name: source.name, policy: source.policy, hasRegistryCredential: source.hasRegistryCredential,
    });
    if (source.hasRegistryCredential) {
      yield* copyCredential({ organizationId: project.organizationId, from, lineageId: node.lineageId, to: node.id });
    }
    copies.push({ from: source.id, to: node.id });
  }
  for (const node of arrivingVolumes) {
    const source = resources.find((row) => row.lineageId === node.resourceLineageId);
    if (!source) return yield* Effect.die(`Source volume for lineage ${node.resourceLineageId} is missing.`);
    yield* drizzle.insert(environmentResource).values({
      id: node.resourceId, organizationId: project.organizationId, projectId: project.id, environmentId: to,
      lineageId: node.resourceLineageId, implementationType: "volume",
    });
    copies.push({ from: source.id, to: node.resourceId });
  }
  const copiedPositions = copies.flatMap((copy) => positions
    .filter((row) => row.resourceId === copy.from)
    .map((row) => ({ organizationId: project.organizationId, environmentId: to, resourceType: row.resourceType, resourceId: copy.to, x: row.x, y: row.y })));
  if (copiedPositions.length) yield* drizzle.insert(environmentCanvasNodePosition).values(copiedPositions);
});

/** Each arrived node's Node Introduction; call after writing the Working State. */
const captureIntroductions = Effect.fn("Branches.captureIntroductions")(function* (
  environmentId: string,
  services: ReadonlyArray<{ id: string }>,
  volumes: ReadonlyArray<{ resourceId: string }>,
) {
  for (const node of services) yield* captureEnvironmentNodeIntroduction({ environmentId, nodeType: "service", nodeId: node.id });
  for (const node of volumes) yield* captureEnvironmentNodeIntroduction({ environmentId, nodeType: "volume", nodeId: node.resourceId });
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
