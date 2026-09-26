import "@tanstack/react-start/server-only";

import { and, desc, eq, sql, type SQL } from "drizzle-orm";
import type { EffectPgDatabase } from "drizzle-orm/effect-postgres";
import { Effect, Schema } from "effect";

import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import type {
  DeploymentAttemptQueryInput,
  DeploymentBuildTailQueryInput,
  DeploymentOperationEvidencePageQueryInput,
  DeploymentServiceVariablesQueryInput,
  EnvironmentChangeStateNodeProjection,
  EnvironmentChangeStateProjection,
  NodeDeploymentsQueryInput,
  EnvironmentDeploymentsQueryInput,
  OrganizationEnvironmentChangeStateQueryInput,
} from "#/modules/deployments/deployment-contract";
import { loadEnvironmentSnapshotProjection, type EnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { decodeEnvironmentResourceNodeConfig } from "#/modules/environment-design/environment-resource-node";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import type { Actor } from "#/modules/identity/actor";
import { environment, project } from "#/modules/project/tables";
import { environmentNodeConfigSnapshot } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

import { loadDeploymentBuildLog, loadDeploymentEvents } from "./deployment-events.server";
import { viewTargetNodes, deploymentView } from "./deployment-view";
import { deploymentRowColumns } from "./deployment-row.server";
import { environmentDeployment } from "./tables";
import { loadDeploymentContext, loadDisplayedDeployEnv, needsClusterDomain } from "./runtime-hydration.repository.server";

const requireOrganization = Effect.fn("Deployments.requireOrganization")(
  function* (actor: Actor, organizationSlug: string) {
    const organization = yield* getOrganizationForUserBySlug(
      actor.userId,
      organizationSlug,
    );
    if (organization !== null) return organization;
    return yield* new NotFound({
      message: "The organization was not found.",
    });
  },
);

function parseEnvironmentChangeStateNode(input: {
  readonly nodeType: "service" | "volume";
  readonly nodeId: string;
  readonly nodeLineageId: string;
  readonly revisionId: string | null;
  readonly config: unknown;
}) {
  const identity = {
    nodeId: input.nodeId,
    nodeLineageId: input.nodeLineageId,
    revisionId: input.revisionId,
  };
  const invalid = () =>
    new Conflict({ message: "Environment change state is invalid." });
  if (input.nodeType === "service") {
    // Sealed variable ciphertext stays on the server; the browser sees each sealed value's fingerprint.
    return Schema.decodeUnknownEffect(serviceDeploymentConfigSchema)(
      withoutSealedCiphertext(input.config),
      strictParseOptions,
    ).pipe(
      Effect.map(
        (config): EnvironmentChangeStateNodeProjection => ({
          ...identity,
          nodeType: "service",
          config,
        }),
      ),
      Effect.mapError(invalid),
    );
  }
  const nodeType = input.nodeType;
  return decodeEnvironmentResourceNodeConfig(nodeType, input.config).pipe(
    Effect.map((decoded) => ({
      ...identity,
      ...decoded,
    })),
    Effect.mapError(invalid),
  );
}

type EnvironmentChangeStateEvidenceNode = NonNullable<
  EnvironmentChangeStateProjection["deploymentEvidence"]
>["nodes"][number];

function parseEvidenceChangeStateNode(input: {
  readonly nodeType: "service" | "volume";
  readonly nodeId: string;
  readonly nodeLineageId: string;
  readonly revisionId: string | null;
  readonly config: unknown;
}): Effect.Effect<EnvironmentChangeStateEvidenceNode, Conflict> {
  return input.config === null
    ? Effect.succeed({ ...input, config: null })
    : parseEnvironmentChangeStateNode(input);
}

const projectEnvironmentChangeStateRecords = Effect.fn(
  "Deployments.projectEnvironmentChangeStateRecords",
)(function* (projection: EnvironmentSnapshotProjection) {
  return yield* Effect.forEach(projection.explicitStates, (state) =>
    Effect.gen(function* () {
      const savedNodes = yield* Effect.forEach(
        state.saved?.nodes ?? [],
        parseEnvironmentChangeStateNode,
      );
      const appliedNodes = yield* Effect.forEach(
        state.applied.nodes,
        parseEnvironmentChangeStateNode,
      );
      const evidenceNodes = yield* Effect.forEach(
        state.deploymentEvidence?.nodes ?? [],
        parseEvidenceChangeStateNode,
      );
      return {
        environmentId: state.environmentId,
        saved: state.saved ? { ...state.saved, nodes: savedNodes } : null,
        applied: { ...state.applied, nodes: appliedNodes },
        deploymentEvidence: state.deploymentEvidence
          ? { ...state.deploymentEvidence, nodes: evidenceNodes }
          : null,
      };
    }),
  );
});

export const listLatestOrganizationEnvironmentChangeStates = Effect.fn(
  "Deployments.listLatestOrganizationEnvironmentChangeStates",
)(function* (
  actor: Actor,
  input: OrganizationEnvironmentChangeStateQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const projection = yield* loadEnvironmentSnapshotProjection({
      kind: "organization",
      organizationId: organization.id,
    });
  return yield* projectEnvironmentChangeStateRecords(projection);
});

const logCursor = Effect.fn("Deployments.logCursor")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const after = Number(input.afterSequence ?? 0);
  if (!Number.isSafeInteger(after) || after < 0) return yield* new Validation({ message: "Invalid log cursor." });
  return { organizationId: organization.id, deploymentId: input.deploymentId, after };
});

export const listDeploymentBuildLog = Effect.fn("Deployments.buildLog")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  return yield* loadDeploymentBuildLog({ ...yield* logCursor(actor, input), limit: input.limit ?? 100 });
});

/** Each Build Step's last rows: enough for a node's log tail, however long the build ran. */
export const listDeploymentBuildTail = Effect.fn("Deployments.buildTail")(function* (actor: Actor, input: DeploymentBuildTailQueryInput) {
  return yield* loadDeploymentBuildLog({ ...yield* logCursor(actor, input), limit: 0, tail: 20 });
});

export const listDeploymentProgressLogs = Effect.fn("Deployments.progressLogs")(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  return yield* loadDeploymentEvents(yield* logCursor(actor, input));
});

/**
 * One service's variables as the attempt deployed them, recomputed through deploy's own loader
 * from the attempt's frozen snapshots and producers. Sealed values, and values resolving from them,
 * are null: never decrypted.
 */
export const getDeploymentServiceVariables = Effect.fn("Deployments.serviceVariables")(function* (
  actor: Actor,
  input: DeploymentServiceVariablesQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const context = yield* loadDeploymentContext(input.deploymentId);
  if (context?.organization.id !== organization.id) {
    return yield* new NotFound({ message: "The deployment was not found." });
  }
  // Deploy reserves the Cluster Domain; a read only looks. Once reserved it never changes.
  const clusterDomain = needsClusterDomain(context) ? (yield* loadClusterDomain(organization.id))?.name ?? null : null;
  const env = yield* loadDisplayedDeployEnv(context, clusterDomain);
  return env.get(input.serviceId) ?? {};
});

/** Every paged deployment read's page size. */
const PAGE_SIZE = 20;

/** Attempts older than attempt `before`, in the (created_at, id) order every deployment page reads newest first. */
const olderThan = (before: string) => sql`(${environmentDeployment.createdAt}, ${environmentDeployment.id}) < (
  select previous.created_at, previous.id from ${environmentDeployment} previous where previous.id = ${before}
)`;

/** A page from up to one row past it: the extra row only says another page exists, and `next` continues after the page. */
function page<Row extends { id: string }>(rows: readonly Row[]) {
  const items = rows.slice(0, PAGE_SIZE);
  return { items, next: rows.length > PAGE_SIZE ? items.at(-1)?.id ?? null : null };
}

/**
 * One node's attempts for its service panel, each with its Node Outcome from the frozen target list: a page of History, the
 * attempts that did not leave it unchanged, newest first; and on the first page the Running attempt (the newest that deployed
 * it, so a later failure leaves it in place). History holds the Running attempt too; the panel shows it once.
 */
export const listNodeDeployments = Effect.fn("Deployments.nodeDeployments")(function* (actor: Actor, input: NodeDeploymentsQueryInput) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const table = environmentDeployment;
  /** A page of the attempts whose Target Node List holds the node, older than `before`. */
  const candidates = (before: string | null) => drizzle.select({
    id: table.id, message: table.message, createdAt: table.createdAt, status: table.status, failureMessage: table.failureMessage,
    planned: sql<boolean>`${table.deployPreview} is not null`, targetNodes: table.targetNodes,
    runtimeProgress: deploymentRowColumns.runtimeProgress,
  }).from(table).where(and(
    eq(table.organizationId, organization.id),
    eq(table.environmentId, input.environmentId),
    sql`${table.targetNodes} -> 'nodes' @> ${JSON.stringify([{ nodeId: input.nodeId }])}::jsonb`,
    before === null ? undefined : olderThan(before),
  )).orderBy(desc(table.createdAt), desc(table.id)).limit(PAGE_SIZE).pipe(Effect.map((rows) => rows.map((row) => {
    const { nodes, progress } = viewTargetNodes(row.targetNodes, row.runtimeProgress);
    const view = deploymentView({ deployment: row, progress, nodes });
    const outcome = view.nodes.find((node) => node.nodeId === input.nodeId)?.outcome ?? "unchanged";
    return { id: row.id, message: row.message, createdAt: row.createdAt, outcome };
  })));
  type NodeDeployment = Effect.Success<ReturnType<typeof candidates>>[number];
  /** Up to `count` attempts that `keep` accepts, older than `before`, newest first. */
  const scan = (before: string | null, keep: (attempt: NodeDeployment) => boolean, count: number) => Effect.gen(function* () {
    const found: NodeDeployment[] = [];
    for (let cursor = before; ;) {
      const candidatePage = yield* candidates(cursor);
      for (const attempt of candidatePage) {
        if (keep(attempt)) found.push(attempt);
        if (found.length === count) return found;
      }
      const last = candidatePage.at(-1);
      if (!last || candidatePage.length < PAGE_SIZE) return found;
      cursor = last.id;
    }
  });
  // ponytail: scans back to the node's last deployed attempt; store it per node if nodes go long without deploying.
  const [running = null] = input.before === undefined ? yield* scan(null, (attempt) => attempt.outcome === "deployed", 1) : [];
  const history = yield* scan(input.before ?? null, (attempt) => attempt.outcome !== "unchanged", PAGE_SIZE + 1);
  return { running, ...page(history) };
});

/** The Org Store's deployment row plus the slugs its summary carries. */
function selectAttempts(drizzle: EffectPgDatabase, organizationId: string, where: SQL | undefined) {
  return drizzle
    .select({ ...deploymentRowColumns, projectSlug: project.slug, environmentSlug: environment.namespace })
    .from(environmentDeployment)
    .innerJoin(environment, eq(environment.id, environmentDeployment.environmentId))
    .innerJoin(project, eq(project.id, environment.projectId))
    .where(and(eq(environmentDeployment.organizationId, organizationId), where));
}

/** One page of an environment's attempts, newest first; ties on `created_at` break by id. */
export const listEnvironmentDeployments = Effect.fn("Deployments.listEnvironmentDeployments")(function* (
  actor: Actor,
  input: EnvironmentDeploymentsQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const rows = yield* selectAttempts(drizzle, organization.id, and(
    eq(environmentDeployment.environmentId, input.environmentId),
    input.before === undefined ? undefined : olderThan(input.before),
  )).orderBy(desc(environmentDeployment.createdAt), desc(environmentDeployment.id)).limit(PAGE_SIZE + 1);
  return page(rows);
});

/**
 * One attempt: its row (with its Target Node List) and the service configs it deployed, for the service panel's Details.
 * Null when the organization has no such attempt.
 */
export const getDeploymentAttempt = Effect.fn("Deployments.getDeploymentAttempt")(function* (
  actor: Actor,
  input: DeploymentAttemptQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const [[row], snapshots] = yield* Effect.all([
    selectAttempts(drizzle, organization.id, eq(environmentDeployment.id, input.deploymentId)),
    drizzle.select({ nodeId: environmentNodeConfigSnapshot.nodeId, config: environmentNodeConfigSnapshot.config })
      .from(environmentNodeConfigSnapshot)
      .where(and(
        eq(environmentNodeConfigSnapshot.organizationId, organization.id),
        eq(environmentNodeConfigSnapshot.environmentDeploymentId, input.deploymentId),
        eq(environmentNodeConfigSnapshot.nodeType, "service"),
      )),
  ]);
  if (!row) return null;
  // Sealed variable ciphertext stays on the server.
  return { row, serviceConfigs: snapshots.map((snapshot) => ({ nodeId: snapshot.nodeId, config: withoutSealedCiphertext(snapshot.config) })) };
});
