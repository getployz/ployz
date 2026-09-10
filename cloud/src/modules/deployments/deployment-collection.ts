import { reconcileCollection } from "#/collections/query-collection";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import {
  createLiveQueryCollection,
  eq,
  toArray,
  type Collection,
} from "@tanstack/react-db";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentSavedStateRevisionsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getVolumeRemoveAttemptsCollection,
} from "#/collections/collections";
import { plainRowCollection } from "#/lib/tanstack-db";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  environmentDeploymentSummarySchema,
  type EnvironmentDeploymentSummary,
} from "#/modules/deployments/deployment-contract";
import { parseSdkDeployPreview } from "#/modules/deployments/runtime-preview";

export const getOrganizationDeploymentsCollection = cachedByCollectionScope((organizationSlug, scope) => {
  const deployments = getEnvironmentDeploymentsCollection(organizationSlug, scope);
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  const projects = getProjectsCollection(organizationSlug, scope);
  const nodeSnapshots =
    getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope);
  const volumeRemoveAttempts = getVolumeRemoveAttemptsCollection(organizationSlug, scope);

  const rows = createLiveQueryCollection({
    id: `collections:${organizationSlug}:deployment-relationships`,
    gcTime: 1,
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
  });

  const collection: Collection<EnvironmentDeploymentSummary> =
    plainRowCollection(
      createLiveQueryCollection({
        id: `collections:${organizationSlug}:deployment-summaries`,
    gcTime: 1,
    query: (q) =>
      q.from({ deploymentRelationships: rows }).fn.select(({ deploymentRelationships }) => {
        const deployment = deploymentRelationships.deployment;
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
            canRetry:
              deployment.status === "failed" &&
              deploymentRelationships.volumeRemoveAttempts.length === 0,
            failureCode: deployment.failureCode,
            dispatchRequestedAt: deployment.dispatchRequestedAt,
            startedAt: deployment.startedAt,
            finishedAt: deployment.finishedAt,
            createdAt: deployment.createdAt,
            updatedAt: deployment.updatedAt,
            serviceCount: deploymentRelationships.nodeSnapshots.filter(
              (snapshot) => snapshot.nodeType === "service",
            ).length,
            projectSlug: deploymentRelationships.projectSlug,
            environmentSlug: deploymentRelationships.environmentSlug,
            volumeRemoveAttempts:
              deploymentRelationships.volumeRemoveAttempts.map((attempt) => ({
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
      }),
    );

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
