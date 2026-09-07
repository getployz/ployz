import {
  createLiveQueryCollection,
  eq,
  toArray,
  type Collection,
} from "@tanstack/react-db";
import {
  getDestructiveVolumeAttemptsCollection,
  getEnvironmentDeploymentsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
} from "#/electric/collections";
import { plainRowCollection } from "#/lib/tanstack-db";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  environmentDeploymentSummarySchema,
  type EnvironmentDeploymentSummary,
} from "#/modules/deployments/deployment-contract";
import { parseSdkDeployPreview } from "#/modules/deployments/runtime-preview";

const cache = new Map<string, Collection<EnvironmentDeploymentSummary>>();

export function getOrganizationDeploymentsCollection(organizationSlug: string) {
  const existing = cache.get(organizationSlug);
  if (existing) return existing;

  const deployments = getEnvironmentDeploymentsCollection(organizationSlug);
  const environments = getEnvironmentsCollection(organizationSlug);
  const projects = getProjectsCollection(organizationSlug);
  const nodeSnapshots =
    getEnvironmentNodeConfigSnapshotsCollection(organizationSlug);
  const destructiveAttempts =
    getDestructiveVolumeAttemptsCollection(organizationSlug);

  const rows = createLiveQueryCollection({
    id: `electric:${organizationSlug}:deployment-relationships`,
    startSync: true,
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
        destructiveAttempts: toArray(
          q
            .from({ destructiveAttempt: destructiveAttempts })
            .where(({ destructiveAttempt }) =>
              eq(destructiveAttempt.environmentDeploymentId, deployment.id),
            ),
        ),
      })),
  });

  const collection: Collection<EnvironmentDeploymentSummary> =
    plainRowCollection(
      createLiveQueryCollection({
        id: `electric:${organizationSlug}:deployment-summaries`,
    startSync: true,
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
              deploymentRelationships.destructiveAttempts.length === 0,
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
            destructiveVolumeAttempts:
              deploymentRelationships.destructiveAttempts.map((attempt) => ({
                id: attempt.id,
                environmentDeploymentId: attempt.environmentDeploymentId,
                environmentResourceId: attempt.environmentResourceId,
                retryOfAttemptId: attempt.retryOfAttemptId,
                target: attempt.target,
                evidence: attempt.evidence,
                evidenceFingerprint: attempt.evidenceFingerprint,
                disposition: attempt.disposition,
                operationId: attempt.operationId,
                startSequence: attempt.startSequence,
                inngestRunId: attempt.inngestRunId,
                requestPublishedAt: attempt.requestPublishedAt,
                acceptedAt: attempt.acceptedAt,
                terminalEvent: attempt.terminalEvent,
                failure: attempt.failure,
                deadlineAt: attempt.deadlineAt,
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

  cache.set(organizationSlug, collection);
  return collection;
}
