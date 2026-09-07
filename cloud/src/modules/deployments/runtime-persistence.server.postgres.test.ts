import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import * as schema from "#/db/schema";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import {
  persistDeployApplyResult,
  persistSdkDeployPreview,
} from "#/modules/deployments/runtime-repository.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { encodeFrozenDeployInput } from "#/modules/deployments/frozen-input.server";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";
import { InngestClient } from "#/modules/inngest/client";

const encryption = makeSecretEncryption("test-encryption-secret");

const organizationId = "00000000-0000-4000-8000-000000000501";
const userId = "00000000-0000-4000-8000-000000000502";
const projectId = "00000000-0000-4000-8000-000000000503";
const environmentId = "00000000-0000-4000-8000-000000000504";
const priorSavedId = "00000000-0000-4000-8000-000000000505";
const targetSavedId = "00000000-0000-4000-8000-000000000506";
const priorDeploymentId = "00000000-0000-4000-8000-000000000507";
const targetDeploymentId = "00000000-0000-4000-8000-000000000508";
const watchId = "00000000-0000-4000-8000-000000000509";
const apiNodeId = "00000000-0000-4000-8000-000000000510";
const apiLineageId = "00000000-0000-4000-8000-000000000511";
const workerNodeId = "00000000-0000-4000-8000-000000000512";
const workerLineageId = "00000000-0000-4000-8000-000000000513";

const emptySavedIntent = {
  version: 1 as const,
  environmentSlug: "production",
  services: [],
  variableGroups: [],
  volumes: [],
};

const preview = (apiRevision: string, workerRevision: string) => ({
  project_name: "production",
  operations: [],
  warnings: [],
  would_remove: [],
  volumes_to_create: [],
  preserved_volumes: [],
  projection: {
    serving_target_commits: [
      { service_id: "api", namespace_revision_entry_id: apiRevision },
      { service_id: "worker", namespace_revision_entry_id: workerRevision },
    ],
  },
});

function deployment(input: {
  id: string;
  savedStateSnapshotId: string;
  status: "queued" | "applied" | "failed";
  createdAt: Date;
  coreDeployId?: string;
  deployPreview?: ReturnType<typeof preview>;
}) {
  const record = {
    id: input.id,
    organizationId,
    environmentId,
    triggerOrigin: { origin: "manual" as const, actorId: userId },
    savedStateSnapshotId: input.savedStateSnapshotId,
    status: input.status,
    coreDeployId: input.coreDeployId,
    deployPreview: input.deployPreview,
    createdAt: input.createdAt,
    updatedAt: input.createdAt,
  };
  return input.status === "applied" || input.status === "failed"
    ? { ...record, finishedAt: input.createdAt }
    : record;
}

function node(input: {
  deploymentId: string;
  nodeId: string;
  lineageId: string;
  runtimeServiceId: string;
  marker: string;
  createdAt: Date;
}) {
  return {
    organizationId,
    environmentDeploymentId: input.deploymentId,
    environmentId,
    nodeType: "service" as const,
    nodeId: input.nodeId,
    nodeLineageId: input.lineageId,
    configVersion: 1,
    config: { privateDns: input.runtimeServiceId, marker: input.marker },
    createdAt: input.createdAt,
    updatedAt: input.createdAt,
  };
}

describe("deployment runtime persistence", () => {
  let harness: GithubPostgresTestHarness;

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Runtime', 'runtime');
      insert into "user" (id, email, name)
      values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (
        id, project_id, organization_id, name, namespace
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production'
      );
    `);
    await harness.db.insert(schema.environmentSavedStateSnapshot).values([
      {
        id: priorSavedId,
        organizationId,
        environmentId,
        actorId: userId,
        intent: emptySavedIntent,
        volumeDeletionAuthorizations: [],
        createdAt: new Date("2026-09-04T01:00:00.000Z"),
      },
      {
        id: targetSavedId,
        organizationId,
        environmentId,
        actorId: userId,
        intent: emptySavedIntent,
        volumeDeletionAuthorizations: [],
        createdAt: new Date("2026-09-04T02:00:00.000Z"),
      },
    ]);
  });

  it("persists preview and promotes a confirmed whole target to Applied", async () => {
    const createdAt = new Date("2026-09-04T03:00:00.000Z");
    await harness.db.insert(schema.environmentDeployment).values(
      deployment({
        id: targetDeploymentId,
        savedStateSnapshotId: targetSavedId,
        status: "queued",
        createdAt,
      }),
    );
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values([
      node({
        deploymentId: targetDeploymentId,
        nodeId: apiNodeId,
        lineageId: apiLineageId,
        runtimeServiceId: "api",
        marker: "target-api",
        createdAt,
      }),
      node({
        deploymentId: targetDeploymentId,
        nodeId: workerNodeId,
        lineageId: workerLineageId,
        runtimeServiceId: "worker",
        marker: "target-worker",
        createdAt,
      }),
    ]);
    const targetPreview = preview("api-revision-2", "worker-revision-2");

    await harness.runEffect(
      persistSdkDeployPreview({
        environmentDeploymentId: targetDeploymentId,
        preview: targetPreview,
      }).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const [previewRow] = await harness.db
      .select({ deployPreview: schema.environmentDeployment.deployPreview })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(previewRow?.deployPreview).toEqual(targetPreview);

    await harness.runEffect(
      persistDeployApplyResult({
        environmentDeploymentId: targetDeploymentId,
        result: { coreDeployId: "deploy-whole-success" },
      }).pipe(
        Effect.provideService(SecretEncryption, encryption),
        Effect.provideService(
          InngestClient,
          new Inngest({ id: "runtime-persistence-test" }),
        ),
      ),
    );
    const projection = await harness.runEffect(
      loadEnvironmentSnapshotProjection(
        { kind: "environment", environmentId },
      ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const explicit = projection.explicitStates[0];

    expect(explicit?.applied.nodes).toEqual([
      expect.objectContaining({
        nodeId: apiNodeId,
        config: expect.objectContaining({ marker: "target-api" }),
        revisionId: "api-revision-2",
      }),
      expect.objectContaining({
        nodeId: workerNodeId,
        config: expect.objectContaining({ marker: "target-worker" }),
        revisionId: "worker-revision-2",
      }),
    ]);
    expect(
      await harness.db
        .select({ status: schema.environmentDeployment.status })
        .from(schema.environmentDeployment)
        .where(eq(schema.environmentDeployment.id, targetDeploymentId)),
    ).toEqual([{ status: "applied" }]);
  });

  it("folds confirmed partial phase evidence without advancing a failed Service", async () => {
    const priorAt = new Date("2026-09-04T03:00:00.000Z");
    const targetAt = new Date("2026-09-04T04:00:00.000Z");
    await harness.db.insert(schema.environmentDeployment).values([
      deployment({
        id: priorDeploymentId,
        savedStateSnapshotId: priorSavedId,
        status: "applied",
        deployPreview: preview("api-revision-1", "worker-revision-1"),
        createdAt: priorAt,
      }),
      deployment({
        id: targetDeploymentId,
        savedStateSnapshotId: targetSavedId,
        status: "failed",
        coreDeployId: "deploy-partial",
        deployPreview: preview("api-revision-2", "worker-revision-2"),
        createdAt: targetAt,
      }),
    ]);
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values([
      node({
        deploymentId: priorDeploymentId,
        nodeId: apiNodeId,
        lineageId: apiLineageId,
        runtimeServiceId: "api",
        marker: "prior-api",
        createdAt: priorAt,
      }),
      node({
        deploymentId: priorDeploymentId,
        nodeId: workerNodeId,
        lineageId: workerLineageId,
        runtimeServiceId: "worker",
        marker: "prior-worker",
        createdAt: priorAt,
      }),
      node({
        deploymentId: targetDeploymentId,
        nodeId: apiNodeId,
        lineageId: apiLineageId,
        runtimeServiceId: "api",
        marker: "target-api",
        createdAt: targetAt,
      }),
      node({
        deploymentId: targetDeploymentId,
        nodeId: workerNodeId,
        lineageId: workerLineageId,
        runtimeServiceId: "worker",
        marker: "target-worker",
        createdAt: targetAt,
      }),
    ]);
    const encryptedFrozenDeployInput = await Effect.runPromise(
      encodeFrozenDeployInput(encryption, {
        version: 3,
        request: {
          version: 1,
          target: {
            namespace_id: "production",
            volumes: {},
            services: [
              {
                service_id: "api",
                image: "api:2",
                mode: { kind: "global" },
                runtime: {
                  command: null,
                  entrypoint: null,
                  environment: {},
                  stop_grace_period: 0,
                },
              },
              {
                service_id: "worker",
                image: "worker:2",
                mode: { kind: "global" },
                runtime: {
                  command: null,
                  entrypoint: null,
                  environment: {},
                  stop_grace_period: 0,
                },
              },
            ],
          },
          phases: [
            {
              services: [
                { service_id: "api", requirement: "required" },
                { service_id: "worker", requirement: "opportunistic" },
              ],
            },
          ],
        },
        registryCredentials: {},
        volumeCount: 0,
      }),
    );
    await harness.db.insert(schema.environmentDeploymentSecret).values({
      environmentDeploymentId: targetDeploymentId,
      encryptedFrozenDeployInput,
    });
    await harness.db.insert(schema.coreOperationWatch).values({
      id: watchId,
      organizationId,
      operationId: "deploy-partial",
      expectedKind: "deploy",
      startSequence: "0",
      nextSequence: "2",
      cursorState: "terminal",
      observationState: "core_terminal",
      deadlineAt: new Date("2026-09-04T05:00:00.000Z"),
      terminalAt: new Date("2026-09-04T04:01:00.000Z"),
    });
    await harness.db.insert(schema.coreOperationEvent).values({
      watchId,
      sequence: "1",
      eventType: "deploy_phase_finished",
      payload: {
        operationId: "deploy-partial",
        phase: 0,
        outcome: "partial",
        services: [
          { serviceId: "api", result: "completed" },
          {
            serviceId: "worker",
            result: "failed",
            failure: { kind: "healthcheck_failed" },
          },
        ],
      },
    });

    const projection = await harness.runEffect(
      loadEnvironmentSnapshotProjection(
        { kind: "environment", environmentId },
      ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const applied = projection.explicitStates[0]?.applied.nodes;

    expect(applied).toEqual([
      expect.objectContaining({
        nodeId: apiNodeId,
        config: expect.objectContaining({ marker: "target-api" }),
        revisionId: "api-revision-2",
      }),
      expect.objectContaining({
        nodeId: workerNodeId,
        config: expect.objectContaining({ marker: "prior-worker" }),
        revisionId: "worker-revision-1",
      }),
    ]);
  });
});
