import { reconcileCollection } from "#/collections/query-collection";
import { cachedByCollectionScope, getDbClient, type CollectionScope } from "#/collections/scope";
import { collectionOptions, eq, liveQueryCollectionOptions, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useQuery, useSuspenseInfiniteQuery } from "@tanstack/react-query";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { viewTargetNodes, deploymentView, type TargetNode, type BuildLog, type DeploymentView } from "#/modules/deployments/deployment-view";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
} from "#/collections/collections";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  environmentDeploymentSummarySchema,
  type EnvironmentDeploymentSummary,
} from "#/modules/deployments/deployment-contract";
import { parseSdkDeployPreview } from "#/modules/deployments/runtime-preview";
import { useBuildTail } from "#/modules/deployments/deployment-build-log.queries";
import {
  deploymentAttemptQueryOptions, environmentDeploymentsQueryOptions, type DeploymentHistoryRow,
} from "#/modules/deployments/deployment-history.queries";

export const getOrganizationDeploymentsCollection = cachedByCollectionScope((organizationSlug, scope) => {
  const client = getDbClient(scope.queryClient);
  const deployments = getEnvironmentDeploymentsCollection(organizationSlug, scope);
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  const projects = getProjectsCollection(organizationSlug, scope);

  const rows = client.collection(collectionOptions(liveQueryCollectionOptions({
    id: `${deployments.id}:deployment-relationships`,
    query: (q) => q
      .from({ deployment: deployments })
      .innerJoin({ environment: environments }, ({ deployment, environment }) =>
        eq(deployment.environmentId, environment.id),
      )
      .innerJoin({ project: projects }, ({ environment, project }) =>
        eq(environment.projectId, project.id),
      )
      .select(({ deployment, environment, project }) => ({
        deployment,
        projectSlug: project.slug,
        environmentSlug: environment.namespace,
      })),
  })));

  const collection =
    client.collection(collectionOptions(liveQueryCollectionOptions({
        id: `${deployments.id}:deployment-summaries`,
    query: (q) =>
      q.from({ deploymentRelationships: rows }).fn.select(({ deploymentRelationships }) => deploymentSummary({
        ...deploymentRelationships.deployment,
        projectSlug: deploymentRelationships.projectSlug,
        environmentSlug: deploymentRelationships.environmentSlug,
      })),
        getKey: (item) => item.id,
      })));

  return collection;
});

/** Maps a deployment row, from the Org Store or a history read, to the summary every view reads. */
function deploymentSummary(deployment: DeploymentHistoryRow): EnvironmentDeploymentSummary {
  const decoded = decodeStrict(environmentDeploymentSummarySchema, {
    id: deployment.id,
    environmentId: deployment.environmentId,
    triggerOrigin: deployment.triggerOrigin,
    status: deployment.status,
    message: deployment.message,
    failureMessage: deployment.failureMessage,
    inngestRunId: deployment.inngestRunId,
    coreDeployId: deployment.coreDeployId,
    deployPreview: deployment.deployPreview,
    runtimeProgress: deployment.runtimeProgress,
    sourcePins: deployment.sourcePins,
    targetNodes: deployment.targetNodes,
    canRetry: deployment.canRetry,
    failureCode: deployment.failureCode,
    dispatchRequestedAt: deployment.dispatchRequestedAt,
    startedAt: deployment.startedAt,
    finishedAt: deployment.finishedAt,
    cancellationRequestedAt: deployment.cancellationRequestedAt,
    createdAt: deployment.createdAt,
    updatedAt: deployment.updatedAt,
    projectSlug: deployment.projectSlug,
    environmentSlug: deployment.environmentSlug,
  });
  return { ...decoded, deployPreview: deployment.deployPreview === null ? null : parseSdkDeployPreview(deployment.deployPreview) };
}

/** Admission and Saved State commands can also replace a queued attempt's history. */
export async function reconcileDeploymentCollections(organizationSlug: string, scope: CollectionScope) {
  await reconcileCollection(getEnvironmentDeploymentsCollection(organizationSlug, scope));
}

export type DeploymentAttempt = { deployment: EnvironmentDeploymentSummary; nodes: TargetNode[]; view: DeploymentView };
/** The attempt Deployment Mode shows. `buildPending`: the build tail is still on its way, so build nodes' stages are unknown yet. */
export type ViewedAttempt = DeploymentAttempt & { buildPending: boolean };

/** An environment's attempts in the Org Store, newest first. */
function useStoredAttempts(organizationSlug: string, environmentId: string) {
  const summaries = getOrganizationDeploymentsCollection(organizationSlug, useCollectionScope());
  const { data } = useLiveSuspenseQuery({
    queryKey: ["environment-deployment-attempts", summaries.id, environmentId],
    query: (q) => q.from({ deployment: summaries }).where(({ deployment }) => eq(deployment.environmentId, environmentId))
      .orderBy(({ deployment }) => deployment.createdAt, "desc"),
  });
  return data;
}

/** One attempt through the deployment view projection. */
function viewAttempt(deployment: EnvironmentDeploymentSummary, buildLog?: BuildLog | null): DeploymentAttempt {
  const { nodes, progress } = viewTargetNodes(deployment.targetNodes, deployment.runtimeProgress);
  const view = deploymentView({ deployment: { ...deployment, planned: deployment.deployPreview !== null }, progress, nodes, buildLog });
  return { deployment, nodes, view };
}

/**
 * The attempt Deployment Mode shows, through the deployment view projection; null when the environment has no such attempt.
 * An attempt the Org Store holds needs no read; one outside it comes from its per-attempt Remote Read, `pending` until it
 * arrives. `buildLog` also reads the attempt's Build Steps and output tails (polled until it finishes) for per-image build
 * stages and tails.
 */
export function useDeploymentAttempt(organizationSlug: string, environmentId: string, deploymentId: string | null, { buildLog = false } = {}) {
  const stored = useStoredAttempts(organizationSlug, environmentId).find((candidate) => candidate.id === deploymentId);
  // useQuery, not useSuspenseQuery: the header, deploy bar and inspector read this attempt too and must stay mounted while
  // it loads, so only the canvas nodes wait (on `pending`). A failed read (not a missing attempt) still fails the route.
  const { data: read, isPending } = useQuery({ ...deploymentAttemptQueryOptions(organizationSlug, stored ? null : deploymentId), throwOnError: true });
  const deployment = stored ?? (read?.row.environmentId === environmentId ? deploymentSummary(read.row) : undefined);
  const tailId = buildLog && deployment?.targetNodes.nodes.some((node) => node.needsBuild) ? deployment.id : null;
  const tail = useBuildTail(organizationSlug, tailId);
  const attempt: ViewedAttempt | null = deployment ? { ...viewAttempt(deployment, tail.data), buildPending: tailId !== null && tail.isPending } : null;
  return { attempt, pending: !deployment && isPending };
}

/** The attempts of an environment the Org Store holds (active ones plus the latest) through the deployment view projection, newest first. */
export function useEnvironmentDeployments(organizationSlug: string, environmentId: string): DeploymentAttempt[] {
  return useStoredAttempts(organizationSlug, environmentId).map((deployment) => viewAttempt(deployment));
}

/**
 * The environment's deployment list, a server page at a time, newest first. A row the Org Store holds shows its live status.
 * Suspends until the first page arrives.
 */
export function useDeploymentList(organizationSlug: string, environmentId: string) {
  const stored = new Map(useStoredAttempts(organizationSlug, environmentId).map((deployment) => [deployment.id, deployment]));
  const { data, hasNextPage, isFetchingNextPage, fetchNextPage } = useSuspenseInfiniteQuery(environmentDeploymentsQueryOptions(organizationSlug, environmentId));
  const attempts = data.pages.flatMap((page) => page.items).map((row) => viewAttempt(stored.get(row.id) ?? deploymentSummary(row)));
  return { attempts, hasMore: hasNextPage, loadingMore: isFetchingNextPage, showMore: () => void fetchNextPage() };
}
