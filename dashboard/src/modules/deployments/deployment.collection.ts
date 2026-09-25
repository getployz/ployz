import { reconcileCollection } from "#/collections/query-collection";
import { cachedByCollectionScope, getDbClient, type CollectionScope } from "#/collections/scope";
import {
  collectionOptions, liveQueryCollectionOptions,
  eq,
  toArray,
  useLiveSuspenseQuery,
} from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { attemptTarget, deploymentView, type AttemptTargetNode, type BuildLog, type DeploymentView } from "#/modules/deployments/deployment-view";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentSavedStateRevisionsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getVolumeRemoveAttemptsCollection,
} from "#/collections/collections";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  environmentDeploymentSummarySchema,
  type EnvironmentDeploymentSummary,
} from "#/modules/deployments/deployment-contract";
import { parseServiceConfig } from "@ployz/sdk/config";
import { parseSdkDeployPreview } from "#/modules/deployments/runtime-preview";
import { useBuildLog } from "#/modules/deployments/deployment-build-log.queries";

export const getOrganizationDeploymentsCollection = cachedByCollectionScope((organizationSlug, scope) => {
  const client = getDbClient(scope.queryClient);
  const deployments = getEnvironmentDeploymentsCollection(organizationSlug, scope);
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  const projects = getProjectsCollection(organizationSlug, scope);
  const nodeSnapshots =
    getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope);
  const volumeRemoveAttempts = getVolumeRemoveAttemptsCollection(organizationSlug, scope);

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
        nodeSnapshots: toArray(
          q
            .from({ snapshot: nodeSnapshots })
            .where(({ snapshot }) =>
              eq(snapshot.environmentDeploymentId, deployment.id),
            ),
        ),
        volumeRemoveAttempts: toArray(
          q
            .from({ volumeRemoveAttempt: volumeRemoveAttempts })
            .where(({ volumeRemoveAttempt }) =>
              eq(volumeRemoveAttempt.environmentDeploymentId, deployment.id),
            ),
        ),
      })),
  })));

  const collection =
    client.collection(collectionOptions(liveQueryCollectionOptions({
        id: `${deployments.id}:deployment-summaries`,
    query: (q) =>
      q.from({ deploymentRelationships: rows }).fn.select(({ deploymentRelationships }) => {
        const deployment = deploymentRelationships.deployment;
        const snapshots = deploymentRelationships.nodeSnapshots ?? [];
        const volumeAttempts = deploymentRelationships.volumeRemoveAttempts ?? [];
        const decoded = decodeStrict(
          environmentDeploymentSummarySchema,
          {
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
            buildServiceIds: snapshots.filter((snapshot) => snapshot.nodeType === "service" && parseServiceConfig(snapshot.config).source.type === "git").map((snapshot) => snapshot.nodeId),
            canRetry:
              deployment.status === "failed" &&
              volumeAttempts.length === 0,
            failureCode: deployment.failureCode,
            dispatchRequestedAt: deployment.dispatchRequestedAt,
            startedAt: deployment.startedAt,
            finishedAt: deployment.finishedAt,
            cancellationRequestedAt: deployment.cancellationRequestedAt,
            createdAt: deployment.createdAt,
            updatedAt: deployment.updatedAt,
            serviceCount: snapshots.filter(
              (snapshot) => snapshot.nodeType === "service",
            ).length,
            projectSlug: deploymentRelationships.projectSlug,
            environmentSlug: deploymentRelationships.environmentSlug,
            volumeRemoveAttempts:
              volumeAttempts.map((attempt) => ({
                id: attempt.id,
                environmentDeploymentId: attempt.environmentDeploymentId,
                environmentResourceId: attempt.environmentResourceId,
                retryOfAttemptId: attempt.retryOfAttemptId,
                volumes: attempt.volumes,
                status: attempt.status,
                inngestRunId: attempt.inngestRunId,
                outcome: attempt.outcome,
                failureMessage: attempt.failureMessage,
                startedAt: attempt.startedAt,
                terminalAt: attempt.terminalAt,
                createdAt: attempt.createdAt,
                updatedAt: attempt.updatedAt,
              })),
          },
        );
        return {
          ...decoded,
          deployPreview:
            deployment.deployPreview === null
              ? null
              : parseSdkDeployPreview(deployment.deployPreview),
        } satisfies EnvironmentDeploymentSummary;
      }),
        getKey: (item) => item.id,
      })));

  return collection;
});

/** Admission and Saved State commands can also replace a queued attempt's history. */
export async function reconcileDeploymentCollections(organizationSlug: string, scope: CollectionScope) {
  await Promise.all([
    reconcileCollection(getEnvironmentDeploymentsCollection(organizationSlug, scope)),
    reconcileCollection(getEnvironmentSavedStateRevisionsCollection(organizationSlug, scope)),
    reconcileCollection(getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope)),
    reconcileCollection(getVolumeRemoveAttemptsCollection(organizationSlug, scope)),
  ]);
}

export type DeploymentAttempt = { deployment: EnvironmentDeploymentSummary; nodes: AttemptTargetNode[]; view: DeploymentView };

/** An environment's attempts newest first, with the attempt rows and node snapshots `attemptTarget` reads besides the attempt itself. */
function useEnvironmentAttemptInputs(organizationSlug: string, environmentId: string) {
  const scope = useCollectionScope();
  const summaries = getOrganizationDeploymentsCollection(organizationSlug, scope);
  const deployments = getEnvironmentDeploymentsCollection(organizationSlug, scope);
  const snapshots = getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope);
  const { data: attempts } = useLiveSuspenseQuery({
    queryKey: ["environment-deployment-attempts", summaries.id, environmentId],
    query: (q) => q.from({ deployment: summaries }).where(({ deployment }) => eq(deployment.environmentId, environmentId))
      .orderBy(({ deployment }) => deployment.createdAt, "desc"),
  });
  const { data: history } = useLiveSuspenseQuery({
    queryKey: ["deployment-attempt-history", deployments.id, environmentId],
    query: (q) => q.from({ deployment: deployments }).where(({ deployment }) => eq(deployment.environmentId, environmentId))
      .select(({ deployment }) => ({ id: deployment.id, status: deployment.status, createdAt: deployment.createdAt })),
  });
  const { data: snapshotRows } = useLiveSuspenseQuery({
    queryKey: ["deployment-attempt-snapshots", snapshots.id, environmentId],
    query: (q) => q.from({ snapshot: snapshots }).where(({ snapshot }) => eq(snapshot.environmentId, environmentId))
      .select(({ snapshot }) => ({ environmentDeploymentId: snapshot.environmentDeploymentId, nodeType: snapshot.nodeType, nodeId: snapshot.nodeId, config: snapshot.config })),
  });
  const project = (deployment: EnvironmentDeploymentSummary, buildLog?: BuildLog | null): DeploymentAttempt => {
    const { nodes, progress } = attemptTarget({ attempt: deployment, progress: deployment.runtimeProgress, history, snapshots: snapshotRows });
    return { deployment, nodes, view: deploymentView({ deployment, progress, nodes, buildLog }) };
  };
  return { attempts, project };
}

/**
 * One Cloud Deployment Attempt of an environment through the deployment view projection; null when the environment has no such attempt.
 * `buildLog` also reads the attempt's Build Steps (polled until it finishes) for per-image build stages and tails.
 */
export function useDeploymentAttempt(organizationSlug: string, environmentId: string, deploymentId: string | null, { buildLog = false } = {}): DeploymentAttempt | null {
  const { attempts, project } = useEnvironmentAttemptInputs(organizationSlug, environmentId);
  const deployment = attempts.find((candidate) => candidate.id === deploymentId);
  const { data: log } = useBuildLog(organizationSlug, buildLog && deployment?.buildServiceIds.length ? deployment.id : null);
  return deployment ? project(deployment, log) : null;
}

/** Every Cloud Deployment Attempt of an environment through the deployment view projection, newest first. */
// ponytail: projects every attempt on each change; page the history if environments grow long ones.
export function useEnvironmentDeployments(organizationSlug: string, environmentId: string): DeploymentAttempt[] {
  const { attempts, project } = useEnvironmentAttemptInputs(organizationSlug, environmentId);
  return attempts.map((deployment) => project(deployment));
}

/** The environment's attempts whose target holds this node, newest first, each with the node's view. */
export function useNodeDeployments(organizationSlug: string, environmentId: string, nodeId: string) {
  return useEnvironmentDeployments(organizationSlug, environmentId).flatMap(({ deployment, view }) => {
    const node = view.nodes.find((candidate) => candidate.nodeId === nodeId);
    return node ? [{ deployment, node }] : [];
  });
}
