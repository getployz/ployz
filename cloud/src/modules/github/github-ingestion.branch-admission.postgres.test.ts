import { randomUUID } from "node:crypto";
import { emptyEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { eq, sql } from "drizzle-orm";
import { Inngest } from "inngest";
import * as schema from "#/db/schema";
import { Effect, Result as EffectResult } from "effect";
import { planGithubBranchEvaluation } from "#/modules/github/github-branch-evaluation";
import type { GithubBranchCursor } from "#/modules/github/github-ingestion.repository";
import {
  runGithubRepositoryResult as runGithubRepositoryResultWithHarness,
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import * as repository from "#/modules/github/github-ingestion.repository";
import { InngestClient } from "#/modules/inngest/client";
import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createGitServiceSource,
  projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";

const organizationId = "00000000-0000-4000-8000-000000000101";
const userId = "00000000-0000-4000-8000-000000000102";
const projectId = "00000000-0000-4000-8000-000000000103";
const environmentId = "00000000-0000-4000-8000-000000000104";
const lineageId = "00000000-0000-4000-8000-000000000105";
const serviceId = "00000000-0000-4000-8000-000000000106";
const variableId = "00000000-0000-4000-8000-000000000108";
const savedStateSnapshotId = "00000000-0000-4000-8000-000000000109";
const volumeLineageId = "00000000-0000-4000-8000-000000000110";
const volumeId = "00000000-0000-4000-8000-000000000111";
const replacementLineageId = "00000000-0000-4000-8000-000000000117";
const replacementServiceId = "00000000-0000-4000-8000-000000000118";
const laterEnvironmentId = "00000000-0000-4000-8000-000000000120";
const laterLineageId = "00000000-0000-4000-8000-000000000121";
const laterServiceId = "00000000-0000-4000-8000-000000000122";
const installationId = 17;
const repositoryId = 42;

const gitSource = createGitServiceSource({
  repository: "acme/api",
  repositoryId,
  installationId,
});

function savedServiceConfig(name = "Saved API") {
  return projectServiceDeploymentConfig({
    name,
    source: gitSource,
    preDeployCommand: null,
    startCommand: null,
    healthcheck: createDefaultServiceHealthcheck(),
    restartPolicy: createDefaultServiceRestartPolicy(),
    privateDns: "api",
    build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
    env: {
      SAVED_ONLY: { kind: "literal", value: "published" },
    },
  });
}

function savedIntent(
  services: Array<{
    id: string;
    lineageId: string;
    config: ReturnType<typeof savedServiceConfig>;
  }>,
  volumes: SavedEnvironmentIntent["volumes"] = [],
): SavedEnvironmentIntent {
  return {
    version: 1,
    environmentSlug: "production",
    services: services.map(({ id, lineageId, config }) => {
      const { env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...authoredConfig } = config;
      void _mounts;
      return {
        id,
        lineageId,
        slug: config.privateDns,
        config: authoredConfig,
        variables: Object.entries(env).map(([key, value]) => {
          if (value.kind === "secret") {
            const encryptedValue = value.encryptedValue;
            if (!encryptedValue) {
              throw new Error("Test secret is missing ciphertext.");
            }
            return {
              id,
              key,
              description: null,
              exported: false,
              valueFingerprint: `test:${key}`,
              value: { kind: "secret" as const, encryptedValue },
            };
          }
          return {
            id,
            key,
            description: null,
            exported: false,
            valueFingerprint: `test:${key}`,
            value: { kind: "literal" as const, value: value.value },
          };
        }),
        variableGroupAttachments: [],
        volumeAttachments: [],
        encryptedRegistryUsername: null,
        encryptedRegistrySecret: null,
      };
    }),
    variableGroups: [],
    volumes,
  };
}

describe("GitHub branch deployment admission", () => {
  let harness: GithubPostgresTestHarness;
  const inngest = new Inngest({ id: "github-branch-admission-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const runGithubRepositoryResult = <Success, Failure>(
    operation: Effect.Effect<
      Success,
      Failure,
      import("#/server/database.server").Database | InngestClient
    >,
  ) =>
    runGithubRepositoryResultWithHarness(
      harness,
      operation.pipe(Effect.provideService(InngestClient, inngest)),
    );

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness.stop();
  });

  beforeEach(async () => {
    const working = savedIntent([{ id: serviceId, lineageId, config: savedServiceConfig("Working API") }]);
    const node = working.services[0];
    if (!node) throw new Error("Working service missing.");
    node.variables = [{ id: variableId, key: "WORKING_ONLY", description: null, exported: false, valueFingerprint: "unfinished", value: { kind: "literal", value: "unfinished" } }];
    await harness.pool.query(`
      truncate table github_environment_trigger, github_branch_projection, github_webhook_delivery, organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acceptance', 'acceptance');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'GitHub', 'github');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        values ('${environmentId}', '${projectId}', '${organizationId}', 'Production', 'production', '${JSON.stringify(working)}');
      insert into service_lineage (id, project_id, canonical_name, canonical_slug) values ('${lineageId}', '${projectId}', 'API', 'api');
      insert into service (id, project_id, environment_id, organization_id, lineage_id)
        values ('${serviceId}', '${projectId}', '${environmentId}', '${organizationId}', '${lineageId}');
      insert into variable (id, environment_id, service_id) values ('${variableId}', '${environmentId}', '${serviceId}');
    `);
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: savedStateSnapshotId,
      organizationId,
      environmentId,
      actorId: userId,
      volumeDeletionAuthorizations: [],
      message: "Published configuration",
      intent: savedIntent(
        [{ id: serviceId, lineageId, config: savedServiceConfig() }],
        [
        {
          resourceId: volumeId,
          resourceLineageId: volumeLineageId,
          name: "Pending removal",
        },
        ],
      ),
    });
  });

  async function admitPush(input: {
    deliveryId: string;
    headSha: string;
    cursor: GithubBranchCursor | null;
    forced?: boolean;
    beforeApply?: () => Promise<void>;
  }) {
    const recorded = await runGithubRepositoryResult(
      repository.recordGithubDelivery({
      deliveryId: input.deliveryId,
      eventKind: "push",
      installationId,
      repositoryId,
      ref: "refs/heads/main",
      branch: { state: "active", headSha: input.headSha },
      }),
    );
    if (EffectResult.isFailure(recorded)) throw recorded.failure;
    const processingRunId = `run-${input.deliveryId}`;
    const claimed = await runGithubRepositoryResult(
      repository.claimGithubDelivery({
      deliveryId: input.deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId,
      }),
    );
    if (EffectResult.isFailure(claimed)) throw claimed.failure;
    const plan = planGithubBranchEvaluation({
      cursor: input.cursor,
      liveBranch: {
        state: "present",
        ref: "refs/heads/main",
        headSha: input.headSha,
      },
      forced: input.forced ?? false,
      comparison: { state: "not_required" },
      candidates: [{ environmentId, serviceId, watchPaths: [] }],
    });
    if (EffectResult.isFailure(plan)) throw plan.failure;
    await input.beforeApply?.();
    return runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
      deliveryId: input.deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId,
      installationId,
      repositoryId,
      ref: "refs/heads/main",
      expectedCursor: input.cursor,
      plan: plan.success,
      }),
    );
  }

  it("save then Git compiles the queued target from Saved, excluding later Working edits", async () => {
    const headSha = "a".repeat(40);
    const admitted = await admitPush({
      deliveryId: "saved-state",
      headSha,
      cursor: null,
      beforeApply: async () => {
        await harness.db
          .update(schema.environment)
          .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,name}', '"Unsaved after candidate selection"')`, revision: randomUUID() })
          .where(eq(schema.environment.id, environmentId));
      },
    });

    expect(EffectResult.isSuccess(admitted)).toBe(true);
    const [deployment] = await harness.db
      .select()
      .from(schema.environmentDeployment);
    const nodes = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot);
    const node = nodes.find(({ nodeType }) => nodeType === "service");
    const [trigger] = await harness.db
      .select()
      .from(schema.githubEnvironmentTrigger);

    expect(deployment).toMatchObject({
      savedStateSnapshotId,
      triggerOrigin: {
        origin: "github",
        deliveryId: "saved-state",
        branchEvaluationRevision: 1,
        installationId,
        repositoryId,
      },
    });
    expect(node?.config).toMatchObject({
      name: "Saved API",
      env: {
        SAVED_ONLY: { kind: "literal", value: "published" },
      },
    });
    expect(node?.config).not.toMatchObject({
      name: "Unsaved after candidate selection",
    });
    expect(node?.config).not.toHaveProperty("env.WORKING_ONLY");
    expect(trigger).toMatchObject({ headSha, serviceIds: [serviceId] });
  });

  it("discovers Git candidates from the latest Saved snapshot without Working rows", async () => {
    await harness.db
      .delete(schema.service)
      .where(eq(schema.service.id, serviceId));

    const candidates = await runGithubRepositoryResult(
      repository.listGithubServiceCandidates({
      installationId,
      repositoryId,
      ref: "refs/heads/main",
      }),
    );

    expect(EffectResult.isSuccess(candidates) && candidates.success).toEqual([
      { environmentId, serviceId, watchPaths: [] },
    ]);
  });

  it("reselects source overlays from the locked latest Saved snapshot", async () => {
    const latestSavedStateSnapshotId = "00000000-0000-4000-8000-000000000119";
    const admitted = await admitPush({
      deliveryId: "saved-between-steps",
      headSha: "e".repeat(40),
      cursor: null,
      beforeApply: async () => {
        await harness.db.insert(schema.environmentSavedStateSnapshot).values({
          id: latestSavedStateSnapshotId,
          organizationId,
          environmentId,
          actorId: userId,
          volumeDeletionAuthorizations: [],
          message: "Saved between candidate selection and apply",
          createdAt: new Date("2099-01-05T00:00:00.000Z"),
          intent: savedIntent([
            {
              id: serviceId,
              lineageId,
              config: {
                ...savedServiceConfig("Repointed API"),
                source: {
                  version: 1,
                  type: "image",
                  image: "ghcr.io/acme/api:stable",
                  autoUpdate: { type: "off" },
                  credentials: { type: "none" },
                },
              },
            },
            {
              id: replacementServiceId,
              lineageId: replacementLineageId,
              config: {
                ...savedServiceConfig("Replacement API"),
                privateDns: "replacement-api",
              },
            },
          ]),
        });
      },
    });

    expect(
      EffectResult.isSuccess(admitted) && admitted.success.triggersCreated,
    ).toBe(1);
    const [deployment] = await harness.db
      .select()
      .from(schema.environmentDeployment);
    const [trigger] = await harness.db
      .select()
      .from(schema.githubEnvironmentTrigger);
    expect(deployment?.savedStateSnapshotId).toBe(latestSavedStateSnapshotId);
    expect(trigger?.serviceIds).toEqual([replacementServiceId]);
    expect(trigger?.headSha).toBe("e".repeat(40));
  });

  it("includes a newly Saved matching environment when apply follows candidate selection", async () => {
    const admitted = await admitPush({
      deliveryId: "environment-saved-between-steps",
      headSha: "f".repeat(40),
      cursor: null,
      beforeApply: async () => {
        await harness.db.insert(schema.environment).values({
          id: laterEnvironmentId,
          projectId,
          organizationId,
          name: "Staging",
          namespace: "staging",
          intent: emptyEnvironmentIntent("staging"),
        });
        await harness.db.insert(schema.environmentSavedStateSnapshot).values({
          organizationId,
          environmentId: laterEnvironmentId,
          actorId: userId,
          volumeDeletionAuthorizations: [],
          message: "First matching Staging service",
          intent: savedIntent([
            {
              id: laterServiceId,
              lineageId: laterLineageId,
              config: savedServiceConfig("Staging API"),
            },
          ]),
        });
      },
    });

    expect(
      EffectResult.isSuccess(admitted) && admitted.success.triggersCreated,
    ).toBe(2);
    const triggers = await harness.db
      .select()
      .from(schema.githubEnvironmentTrigger);
    const deployments = await harness.db
      .select()
      .from(schema.environmentDeployment);
    expect(
      triggers.map(({ environmentId: id, serviceIds }) => ({ id, serviceIds })),
    ).toEqual([
      { id: environmentId, serviceIds: [serviceId] },
      { id: laterEnvironmentId, serviceIds: [laterServiceId] },
    ]);
    expect(deployments.map(({ environmentId: id }) => id).sort()).toEqual([
      environmentId,
      laterEnvironmentId,
    ]);
  });

  it("waits for a concurrent Save before selecting the exact latest Saved revision", async () => {
    const latestSavedStateSnapshotId = "00000000-0000-4000-8000-000000000114";
    const saver = await harness.pool.connect();
    try {
      await saver.query("begin");
      await saver.query("select pg_advisory_xact_lock(hashtext($1))", [
        environmentId,
      ]);
      await saver.query(
        `insert into environment_saved_state_snapshot (
          id, organization_id, environment_id, actor_id, message,
          intent, volume_deletion_authorizations, created_at
        ) values ($1, $2, $3, $4, 'Concurrent Save', $5, '[]'::jsonb, $6)`,
        [
          latestSavedStateSnapshotId,
          organizationId,
          environmentId,
          userId,
          JSON.stringify(
            savedIntent([
              {
                id: serviceId,
                lineageId,
                config: savedServiceConfig("Concurrent Saved API"),
              },
            ]),
          ),
          new Date("2099-01-03T00:00:00.000Z"),
        ],
      );

      const admission = admitPush({
        deliveryId: "concurrent-save",
        headSha: "c".repeat(40),
        cursor: null,
      });
      await new Promise((resolve) => setTimeout(resolve, 50));
      await saver.query("commit");
      const admitted = await admission;
      expect(EffectResult.isSuccess(admitted)).toBe(true);
    } finally {
      await saver.query("rollback").catch(() => undefined);
      saver.release();
    }

    const [deployment] = await harness.db
      .select()
      .from(schema.environmentDeployment);
    const [node] = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot);
    expect(deployment?.savedStateSnapshotId).toBe(latestSavedStateSnapshotId);
    expect(node?.config).toMatchObject({ name: "Concurrent Saved API" });
  });

  it("keeps a previously Saved pending removal eligible for a later Git target", async () => {
    const appliedDeploymentId = "00000000-0000-4000-8000-000000000115";
    await harness.db.insert(schema.resourceLineage).values({
      id: volumeLineageId,
      organizationId,
      projectId,
      canonicalName: "Applied volume",
      canonicalSlug: "applied-volume",
    });
    await harness.db.insert(schema.environmentResource).values({
      id: volumeId,
      organizationId,
      projectId,
      environmentId,
      lineageId: volumeLineageId,
      implementationType: "volume",
    });
    await harness.db.insert(schema.environmentDeployment).values({
      id: appliedDeploymentId,
      organizationId,
      environmentId,
      savedStateSnapshotId,
      triggerOrigin: { origin: "manual", actorId: userId },
      status: "applied",
      finishedAt: new Date(),
    });
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values({
      organizationId,
      environmentDeploymentId: appliedDeploymentId,
      environmentId,
      nodeType: "volume",
      nodeId: volumeId,
      nodeLineageId: volumeLineageId,
      configVersion: 2,
      config: {
        version: 2,
        name: "Applied volume",
      },
    });
    const removalSavedStateSnapshotId = "00000000-0000-4000-8000-000000000116";
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: removalSavedStateSnapshotId,
      organizationId,
      environmentId,
      actorId: userId,
      volumeDeletionAuthorizations: [
        {
          target: {
            version: 1,
            resourceId: volumeId,
            namespaceId: "production",
            volumeName: `vol-${volumeId}`,
            machineId: "a".repeat(32),
          },
          evidence: {
            version: 1,
            fingerprint: "reviewed-volume-removal",
            reviewedAt: "2026-08-12T00:00:00.000Z",
            evidence: {
              namespaceId: "production",
              volumeName: `vol-${volumeId}`,
              machineId: "a".repeat(32),
              kind: { kind: "plain" },
              availability: {
                status: "available",
                usedBytes: 0,
                lastWriteUnixSeconds: 0,
              },
              referencingServices: [],
            },
          },
        },
      ],
      message: "Saved volume removal",
      createdAt: new Date("2099-01-04T00:00:00.000Z"),
      intent: savedIntent([
        { id: serviceId, lineageId, config: savedServiceConfig() },
      ]),
    });

    const admitted = await admitPush({
      deliveryId: "pending-removal",
      headSha: "d".repeat(40),
      cursor: null,
    });

    expect(EffectResult.isSuccess(admitted)).toBe(true);
    const deployments = await harness.db
      .select()
      .from(schema.environmentDeployment);
    const queued = deployments.find(({ status }) => status === "queued");
    const nodes = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot);
    expect(queued?.savedStateSnapshotId).toBe(removalSavedStateSnapshotId);
    expect(
      nodes.some(
        ({ environmentDeploymentId, nodeId }) =>
          environmentDeploymentId === appliedDeploymentId &&
          nodeId === volumeId,
      ),
    ).toBe(true);
    expect(
      nodes.some(
        ({ environmentDeploymentId, nodeId }) =>
          environmentDeploymentId === queued?.id && nodeId === volumeId,
      ),
    ).toBe(false);
    const attempts = await harness.db
      .select()
      .from(schema.volumeRemoveAttempt);
    expect(attempts).toHaveLength(1);
    expect(attempts[0]).toMatchObject({
      environmentDeploymentId: queued?.id,
      environmentResourceId: volumeId,
      status: "awaiting_deployment",
      requestedByUserId: userId,
      volumes: [{ machine_id: "a".repeat(32), name: `vol-${volumeId}` }],
    });
  });

  it("rebuilds one queued request from the newest Saved and source revisions", async () => {
    const first = await admitPush({
      deliveryId: "coalesced-1",
      headSha: "a".repeat(40),
      cursor: null,
    });
    if (EffectResult.isFailure(first) || !first.success.cursor) {
      throw EffectResult.isFailure(first)
        ? first.failure
        : new Error("Expected the first branch cursor.");
    }
    const [initialDeployment] = await harness.db
      .select()
      .from(schema.environmentDeployment);
    if (!initialDeployment) throw new Error("Expected a queued deployment.");
    const initialNodes = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot);
    expect(initialNodes.map(({ nodeType }) => nodeType).sort()).toEqual([
      "service",
      "volume",
    ]);

    const latestSavedStateSnapshotId = "00000000-0000-4000-8000-000000000112";
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: latestSavedStateSnapshotId,
      organizationId,
      environmentId,
      actorId: userId,
      volumeDeletionAuthorizations: [],
      message: "Published removal",
      createdAt: new Date("2099-01-01T00:00:00.000Z"),
      intent: savedIntent([
        {
          id: serviceId,
          lineageId,
          config: savedServiceConfig("Saved API v2"),
        },
      ]),
    });

    const second = await admitPush({
      deliveryId: "coalesced-2",
      headSha: "b".repeat(40),
      cursor: first.success.cursor,
      forced: true,
    });

    expect(EffectResult.isSuccess(second)).toBe(true);
    const deployments = await harness.db
      .select()
      .from(schema.environmentDeployment);
    const nodes = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot);
    const triggers = await harness.db
      .select()
      .from(schema.githubEnvironmentTrigger);
    expect(deployments).toHaveLength(1);
    expect(deployments[0]).toMatchObject({
      id: initialDeployment.id,
      savedStateSnapshotId: latestSavedStateSnapshotId,
      triggerOrigin: {
        deliveryId: "coalesced-2",
        branchEvaluationRevision: 2,
      },
    });
    expect(nodes).toHaveLength(1);
    expect(nodes[0]).toMatchObject({
      nodeType: "service",
      config: expect.objectContaining({ name: "Saved API v2" }),
    });
    expect(triggers.map(({ headSha }) => headSha)).toEqual([
      "a".repeat(40),
      "b".repeat(40),
    ]);
  });

  it("keeps a running Attempt Target immutable and queues the newest target separately", async () => {
    const first = await admitPush({
      deliveryId: "running-1",
      headSha: "a".repeat(40),
      cursor: null,
    });
    if (EffectResult.isFailure(first) || !first.success.cursor) {
      throw EffectResult.isFailure(first)
        ? first.failure
        : new Error("Expected the first branch cursor.");
    }
    const [running] = await harness.db
      .select()
      .from(schema.environmentDeployment);
    if (!running) throw new Error("Expected a queued deployment.");
    await harness.db
      .update(schema.environmentDeployment)
      .set({ status: "planning", startedAt: new Date() })
      .where(eq(schema.environmentDeployment.id, running.id));

    const latestSavedStateSnapshotId = "00000000-0000-4000-8000-000000000113";
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: latestSavedStateSnapshotId,
      organizationId,
      environmentId,
      actorId: userId,
      volumeDeletionAuthorizations: [],
      message: "Published while running",
      createdAt: new Date("2099-01-02T00:00:00.000Z"),
      intent: savedIntent([
        {
          id: serviceId,
          lineageId,
          config: savedServiceConfig("Queued after running"),
        },
      ]),
    });

    const second = await admitPush({
      deliveryId: "running-2",
      headSha: "b".repeat(40),
      cursor: first.success.cursor,
      forced: true,
    });

    expect(EffectResult.isSuccess(second)).toBe(true);
    const deployments = await harness.db
      .select()
      .from(schema.environmentDeployment);
    const nodes = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot);
    const runningAfter = deployments.find(({ id }) => id === running.id);
    const queued = deployments.find(({ status }) => status === "queued");
    expect(runningAfter).toMatchObject({
      status: "planning",
      savedStateSnapshotId,
      triggerOrigin: { deliveryId: "running-1" },
    });
    expect(queued).toMatchObject({
      savedStateSnapshotId: latestSavedStateSnapshotId,
      triggerOrigin: { deliveryId: "running-2" },
    });
    expect(
      nodes.find(
        ({ environmentDeploymentId, nodeType }) =>
          environmentDeploymentId === running.id && nodeType === "service",
      )?.config,
    ).toMatchObject({ name: "Saved API" });
    expect(
      nodes.find(
        ({ environmentDeploymentId, nodeType }) =>
          environmentDeploymentId === queued?.id && nodeType === "service",
      )?.config,
    ).toMatchObject({ name: "Queued after running" });
  });
});
