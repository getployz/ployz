import { reconcileCollection } from "#/collections/query-collection";
import { cachedByCollectionScope, getDbClient, type CollectionScope } from "#/collections/scope";
import {
  collectionOptions, liveQueryCollectionOptions,
  eq,
  toArray,
} from "@tanstack/react-db";
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
import { parseDashboardServiceConfig } from "#/modules/environment-design/service-config";
import { parseSdkDeployPreview } from "#/modules/deployments/runtime-preview";

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
            status: deployment.status,
            message: deployment.message,
            failureMessage: deployment.failureMessage,
            inngestRunId: deployment.inngestRunId,
            coreDeployId: deployment.coreDeployId,
            deployPreview: deployment.deployPreview,
            runtimeProgress: deployment.runtimeProgress,
            sourcePins: deployment.sourcePins,
            buildServiceIds: snapshots.filter((snapshot) => snapshot.nodeType === "service" && parseDashboardServiceConfig(snapshot.config).source.type === "git").map((snapshot) => snapshot.nodeId),
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
