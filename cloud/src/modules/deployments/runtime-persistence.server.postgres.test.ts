import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { Effect, Redacted } from "effect";
import type { ContainerId, DeployOutcome, ExecutionError } from "@ployz/sdk";
import { resolvedServiceSpecFixture, runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { getPloyzTable } from "#/electric/synced-tables.server";
import { Inngest } from "inngest";
import * as schema from "#/db/schema";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import {
  persistDeployApplyResult,
  persistSdkDeployPreview,
  persistSdkDeployOutcome,
} from "#/modules/deployments/runtime-repository.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";
import { admitEnvironmentDeployment } from "./admission.server";
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
const apiNodeId = "00000000-0000-4000-8000-000000000510";
const apiLineageId = "00000000-0000-4000-8000-000000000511";
const workerNodeId = "00000000-0000-4000-8000-000000000512";
const workerLineageId = "00000000-0000-4000-8000-000000000513";
const retiredNodeId = "00000000-0000-4000-8000-000000000514";
const retiredLineageId = "00000000-0000-4000-8000-000000000515";

const emptySavedIntent = {
  version: 1 as const,
  environmentSlug: "production",
  services: [],
  variableGroups: [],
  volumes: [],
};

const preview = () => ({
  project_name: "production", operations: [], warnings: [], would_remove: [], preserved_volumes: [],
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
        id, project_id, organization_id, name, namespace, intent
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"variableGroups":[],"volumes":[]}'
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

  it("retains a normally admitted outcome and applies the deployment", async () => {
    const admit = () => harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    const admitted = await admit();
    expect((await admit()).id).toBe(admitted.id);
    expect(await harness.db.select().from(schema.environmentDeploymentSecret)).toEqual([
      { environmentDeploymentId: admitted.id, encryptedRuntimeOutcome: null },
    ]);
    const outcome = { version: 1, outcome: { type: "success" as const, completed: [] } };
    await harness.runEffect(persistSdkDeployOutcome({
      environmentDeploymentId: admitted.id, outcome: Redacted.make(outcome),
    }).pipe(Effect.provideService(SecretEncryption, encryption)));
    const privateRows = await harness.db.select().from(schema.environmentDeploymentSecret);
    expect(privateRows).toHaveLength(1);
    const encryptedOutcome = privateRows[0]?.encryptedRuntimeOutcome;
    if (!encryptedOutcome) throw new Error("Runtime evidence was not persisted");
    expect(JSON.parse(encryption.decrypt(encryptedOutcome))).toEqual(outcome);
    await harness.runEffect(persistDeployApplyResult({
      environmentDeploymentId: admitted.id, result: { coreDeployId: "admitted-success" },
    }).pipe(
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })),
    ));
    const [row] = await harness.db.select().from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, admitted.id));
    expect(row?.status).toBe("applied");
  });

  it("retains encrypted partial runtime evidence even after a terminal-state race", async () => {
    await harness.db.insert(schema.environmentDeployment).values(deployment({
      id: targetDeploymentId, savedStateSnapshotId: targetSavedId,
      status: "failed", createdAt: new Date("2026-09-04T03:00:00.000Z"),
    }));
    await harness.db.insert(schema.environmentDeploymentSecret).values({
      environmentDeploymentId: targetDeploymentId,
    });
    const spec = resolvedServiceSpecFixture();
    spec.container.environment = { PASSWORD: "never-publish-outcome" };
    const machineId = runtimeWatchMachineFixture("a".repeat(32), "A").id;
    const operation = { type: "run_container" as const, machine_id: machineId, spec, skip_health_monitor: false };
    const outcome: DeployOutcome<ExecutionError> = {
      type: "failed", completed: [operation],
      failed: { type: "operation", operation: { ...operation, machine_id: runtimeWatchMachineFixture("b".repeat(32), "B").id },
        error: { type: "machine", action: "StartContainer", error: { code: "internal", message: "never-publish-outcome", details: null } } },
      unexecuted: [{ ...operation, machine_id: runtimeWatchMachineFixture("c".repeat(32), "C").id }],
    };
    await harness.runEffect(persistSdkDeployOutcome({
      environmentDeploymentId: targetDeploymentId, outcome: Redacted.make({ version: 1, outcome }),
    }).pipe(Effect.provideService(SecretEncryption, encryption)));
    const [privateRow] = await harness.db.select().from(schema.environmentDeploymentSecret)
      .where(eq(schema.environmentDeploymentSecret.environmentDeploymentId, targetDeploymentId));
    const encryptedOutcome = privateRow?.encryptedRuntimeOutcome;
    if (!encryptedOutcome) throw new Error("Runtime evidence was not persisted");
    expect(JSON.parse(encryption.decrypt(encryptedOutcome))).toEqual({ version: 1, outcome });
    const [publicRow] = await harness.db.select().from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(publicRow?.status).toBe("failed");
    expect(JSON.stringify([privateRow, publicRow])).not.toContain("never-publish-outcome");
    expect(getPloyzTable("environment_deployment_secret")).toBeNull();
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
    const targetPreview = preview();

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
        revisionId: null,
      }),
      expect.objectContaining({
        nodeId: workerNodeId,
        config: expect.objectContaining({ marker: "target-worker" }),
        revisionId: null,
      }),
    ]);
    expect(
      await harness.db
        .select({ status: schema.environmentDeployment.status })
        .from(schema.environmentDeployment)
        .where(eq(schema.environmentDeployment.id, targetDeploymentId)),
    ).toEqual([{ status: "applied" }]);
  });

  it("projects current SDK partial outcomes without advancing a failed Service", async () => {
    const priorAt = new Date("2026-09-04T03:00:00.000Z");
    const targetAt = new Date("2026-09-04T04:00:00.000Z");
    await harness.db.insert(schema.environmentDeployment).values([
      deployment({
        id: priorDeploymentId,
        savedStateSnapshotId: priorSavedId,
        status: "applied",
        deployPreview: preview(),
        createdAt: priorAt,
      }),
      deployment({
        id: targetDeploymentId,
        savedStateSnapshotId: targetSavedId,
        status: "failed",
        coreDeployId: "deploy-partial",
        deployPreview: preview(),
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
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values(node({
      deploymentId: priorDeploymentId, nodeId: retiredNodeId, lineageId: retiredLineageId,
      runtimeServiceId: "retired", marker: "prior-retired", createdAt: priorAt,
    }));
    const machineId = runtimeWatchMachineFixture("a".repeat(32), "A").id;
    const operation = (name: string) => ({
      type: "run_container" as const, machine_id: machineId,
      spec: { ...resolvedServiceSpecFixture(), name }, skip_health_monitor: false,
    });
    const api = operation("api");
    const worker = operation("worker");
    const removal = { type: "remove_container" as const, machine_id: machineId, container_id: "c".repeat(64) as ContainerId };
    const targetPreview = { ...preview(), operations: [api, removal, worker].map((operation, index) => ({
      index, machine_id: machineId, service_name: operation.type === "run_container" ? operation.spec.name : "retired", operation, status: { type: "pending" as const },
    })) };
    await harness.db.update(schema.environmentDeployment).set({ deployPreview: targetPreview })
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    const outcome: DeployOutcome<ExecutionError> = { type: "failed", completed: [api, removal],
      failed: { type: "operation", operation: worker, error: { type: "cancelled" } }, unexecuted: [] };
    await harness.db.insert(schema.environmentDeploymentSecret).values({
      environmentDeploymentId: targetDeploymentId,
      encryptedRuntimeOutcome: encryption.encrypt(JSON.stringify({ version: 1, outcome })),
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
        revisionId: null,
      }),
      expect.objectContaining({
        nodeId: workerNodeId,
        config: expect.objectContaining({ marker: "prior-worker" }),
        revisionId: null,
      }),
    ]);
  });
});
