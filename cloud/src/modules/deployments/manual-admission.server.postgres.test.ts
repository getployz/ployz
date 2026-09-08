import { randomUUID } from "node:crypto";
import { parseServiceConfig } from "@ployz/sdk/config";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { desc, eq, sql } from "drizzle-orm";
import { Effect, Exit } from "effect";
import { Inngest } from "inngest";
import { SqlError, SerializationError } from "effect/unstable/sql/SqlError";
import * as schema from "#/db/schema";
import { loadCurrentEnvironmentSnapshotProjection } from "#/modules/environment-design/working-state-repository.server";
import { buildEnvironmentChangeSet } from "#/modules/environment-design/environment-change-set";
import type { EnvironmentNodeProjection } from "#/modules/environment-design/environment-change-set";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { markDeploymentStatus } from "#/modules/deployments/runtime-repository.server";
import {
  createManualEnvironmentDeployment,
} from "#/modules/deployments/manual-admission.server";
import { saveReviewedEnvironmentState } from "#/modules/environment-design/saved-state-operations.server";
import type { ReviewedEnvironmentPublication } from "#/modules/environment-design/working-state-review";
import { fingerprintReviewedEnvironmentWorkingState } from "#/modules/environment-design/working-state-review";
import { savedEnvironmentIntentSchema } from "#/modules/environment-design/saved-intent";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import type { DatabaseService } from "#/server/database.server";
import { Database } from "#/server/database.server";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { InngestClient } from "#/modules/inngest/client";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";

const encryption = makeSecretEncryption("test-encryption-secret");

const organizationId = "00000000-0000-4000-8000-000000000101";
const userId = "00000000-0000-4000-8000-000000000102";
const projectId = "00000000-0000-4000-8000-000000000103";
const environmentId = "00000000-0000-4000-8000-000000000104";
const lineageId = "00000000-0000-4000-8000-000000000105";
const serviceId = "00000000-0000-4000-8000-000000000106";
const variableId = "00000000-0000-4000-8000-000000000108";
const volumeLineageId = "00000000-0000-4000-8000-000000000201";
const volumeId = "00000000-0000-4000-8000-000000000202";
const encrypted = {
  version: 1 as const,
  iv: "iv",
  tag: "tag",
  ciphertext: "ciphertext",
};

function saveManualEnvironmentStateSnapshot(input: {
  readonly environmentId: string;
  readonly actorId: string;
  readonly message: string | null;
  readonly review: ReviewedEnvironmentPublication;
}) {
  return Effect.match(saveReviewedEnvironmentState(input), {
    onFailure: (error) => ({ status: "error" as const, error }),
    onSuccess: (value) => ({ status: "ok" as const, value }),
  }).pipe(Effect.provideService(SecretEncryption, encryption));
}

describe("manual environment saved-state persistence", () => {
  let harness: GithubPostgresTestHarness;
  const inngest = new Inngest({ id: "manual-admission-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    const { env: _env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...config } = parseServiceConfig({ version: 2, name: "API", source: { version: 1, type: "empty", rootDir: "/" }, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: "api" });
    const intent = { version: 1, environmentSlug: "production", variableGroups: [], volumes: [], services: [{
      id: serviceId, lineageId, slug: "api", config, encryptedRegistryUsername: null, encryptedRegistrySecret: null,
      variables: [{ id: variableId, key: "API_TOKEN", description: null, exported: false, valueFingerprint: "fingerprint", value: { kind: "secret", encryptedValue: null } }],
      variableGroupAttachments: [], volumeAttachments: [],
    }] };
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acceptance', 'acceptance');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        values ('${environmentId}', '${projectId}', '${organizationId}', 'Production', 'production', '${JSON.stringify(intent)}');
      insert into service_lineage (id, project_id, canonical_name, canonical_slug)
        values ('${lineageId}', '${projectId}', 'API', 'api');
      insert into service (id, project_id, environment_id, organization_id, lineage_id)
        values ('${serviceId}', '${projectId}', '${environmentId}', '${organizationId}', '${lineageId}');
      insert into service_registry_credential (service_id, encrypted_registry_secret)
        values ('${serviceId}', '${JSON.stringify(encrypted)}');
      insert into variable (id, environment_id, service_id)
        values ('${variableId}', '${environmentId}', '${serviceId}');
      insert into variable_secret (environment_id, variable_id, encrypted_value)
        values ('${environmentId}', '${variableId}', '${JSON.stringify(encrypted)}');
    `);
  });

  async function save(message: string | null) {
    const review = await publicationReview();
    return harness.runTransaction(() =>
      saveManualEnvironmentStateSnapshot(
        {
          environmentId,
          actorId: userId,
          message,
          review,
        }
      ),
    );
  }

  async function reviewFingerprint() {
    return harness.runTransaction(() =>
      loadCurrentEnvironmentSnapshotProjection(
          environmentId
        ).pipe(Effect.map(fingerprintReviewedEnvironmentWorkingState)),
    );
  }

  async function currentSavedStateBasis() {
    const [latestSavedState] = await harness.db
      .select({ id: schema.environmentSavedStateSnapshot.id })
      .from(schema.environmentSavedStateSnapshot)
      .where(
        eq(schema.environmentSavedStateSnapshot.environmentId, environmentId),
      )
      .orderBy(
        desc(schema.environmentSavedStateSnapshot.createdAt),
        desc(schema.environmentSavedStateSnapshot.id),
      )
      .limit(1);

    return latestSavedState
      ? {
          kind: "saved_revision" as const,
          savedStateSnapshotId: latestSavedState.id,
        }
      : { kind: "no_saved_state" as const };
  }

  async function publicationReview() {
    return {
      workingStateFingerprint: await reviewFingerprint(),
      savedStateBasis: await currentSavedStateBasis(),
      destructiveServiceIds: [],
      destructiveVolumeReviews: [],
    };
  }

  async function deploy(message: string) {
    const review = await publicationReview();
    return harness.runTransaction(() =>
      createManualEnvironmentDeployment(
        {
          environmentId,
          actorId: userId,
          message,
          review,
        },
      ),
    );
  }

  it("persists actor, message, and encrypted authoring intent", async () => {
    await save("Saved state");
    const [row] = await harness.db
      .select()
      .from(schema.environmentSavedStateSnapshot)
      .where(
        eq(schema.environmentSavedStateSnapshot.environmentId, environmentId),
      );

    expect(row).toMatchObject({
      environmentId,
      actorId: userId,
      message: "Saved state",
    });
    const intent = decodeStrict(savedEnvironmentIntentSchema, row?.intent);
    expect(intent.services[0]?.variables).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          key: "API_TOKEN",
          value: { kind: "secret", encryptedValue: encrypted },
        }),
      ]),
    );
    expect(intent.services[0]).toMatchObject({
      id: serviceId,
      encryptedRegistrySecret: encrypted,
    });
    expect(
      await harness.db.select().from(schema.environmentDeployment),
    ).toEqual([]);
  });

  it("persists an empty-node saved state", async () => {
    await harness.db.update(schema.environment).set({ intent: { version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [] }, revision: randomUUID() }).where(eq(schema.environment.id, environmentId));
    await save(null);
    const [row] = await harness.db
      .select()
      .from(schema.environmentSavedStateSnapshot);
    expect(decodeStrict(savedEnvironmentIntentSchema, row?.intent).services).toEqual(
      [],
    );
  });

  it("publishes Working as Saved before queueing that exact manual target", async () => {
    const deployment = await deploy("Deploy reviewed state");

    const savedRows = await harness.db
      .select()
      .from(schema.environmentSavedStateSnapshot)
      .where(
        eq(schema.environmentSavedStateSnapshot.environmentId, environmentId),
      );
    const queuedNodes = await harness.db
      .select()
      .from(schema.environmentNodeConfigSnapshot)
      .where(
        eq(
          schema.environmentNodeConfigSnapshot.environmentDeploymentId,
          deployment.environmentDeploymentId,
        ),
      );
    const queuedAuthority = await harness.pool.query<{
      saved_state_snapshot_id: string | null;
      service_action_policy: unknown;
    }>(
      `select saved_state_snapshot_id, service_action_policy
       from environment_deployment
       where id = $1`,
      [deployment.environmentDeploymentId],
    );

    expect(savedRows).toHaveLength(1);
    expect(queuedAuthority.rows).toEqual([
      {
        saved_state_snapshot_id: savedRows[0]?.id,
        service_action_policy: { kind: "all_affected_required" },
      },
    ]);
    expect(savedRows[0]).toMatchObject({
      actorId: userId,
      message: "Deploy reviewed state",
    });
    expect(
      decodeStrict(savedEnvironmentIntentSchema, savedRows[0]?.intent).services[0],
    ).toMatchObject({ id: serviceId, config: { name: "API" } });
    expect(queuedNodes).toEqual([
      expect.objectContaining({
        nodeType: "service",
        nodeId: serviceId,
        config: expect.objectContaining({ name: "API" }),
      }),
    ]);
  });

  it("keeps the exact Saved target pending after a manual attempt fails", async () => {
    const deployment = await deploy("Deploy reviewed state");
    const terminalized = await harness.runEffect(
      markDeploymentStatus({
        environmentDeploymentId: deployment.environmentDeploymentId,
        status: "failed",
        message: "runtime failed",
        failureCode: "runtime_failed",
      }).pipe(
        Effect.provideService(SecretEncryption, encryption),
        Effect.provideService(InngestClient, inngest),
      ),
    );
    expect(terminalized).not.toBeNull();

    const projected = await harness.runEffect(
      loadEnvironmentSnapshotProjection(
        { kind: "environment", environmentId },
      ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const explicit = projected.explicitStates[0];
    expect(explicit).toBeDefined();
    if (!explicit) throw new Error("Expected explicit environment state");
    expect(explicit.deploymentEvidence).toBeNull();
    expect(explicit.applied.nodes).toEqual([]);
    expect(explicit.saved?.nodes).toEqual([
      expect.objectContaining({
        nodeType: "service",
        nodeId: serviceId,
        config: expect.objectContaining({ name: "API" }),
      }),
    ]);

    const nodes = (explicit.saved?.nodes ?? []).map(
      (node) =>
        ({
          node: { type: node.nodeType, id: node.nodeId },
          config: node.config,
        }) as EnvironmentNodeProjection,
    );
    const changeSet = buildEnvironmentChangeSet({
      working: { token: "working:unchanged", nodes },
      saved: explicit.saved
        ? {
            kind: "saved_revision",
            savedStateSnapshotId: explicit.saved.snapshotId,
            token: explicit.saved.token,
            nodes,
          }
        : { kind: "no_saved_state", token: "saved:none", nodes: [] },
      applied: { token: explicit.applied.token, nodes: [] },
      nodeIntroductions: { token: "introductions:none", nodes: [] },
      runtimeObserved: { token: explicit.applied.token, nodes: [] },
    });

    expect(changeSet.unsaved.totalCount).toBe(0);
    expect(changeSet.pending.groups).toMatchObject([
      {
        node: { type: "service", id: serviceId },
        lifecycle: { kind: "create" },
      },
    ]);
  });

  it("reuses the latest Saved revision when Working is unchanged", async () => {
    await save("Reviewed state");

    await deploy("Deploy reviewed state");

    const savedRows = await harness.db
      .select({ id: schema.environmentSavedStateSnapshot.id })
      .from(schema.environmentSavedStateSnapshot)
      .where(
        eq(schema.environmentSavedStateSnapshot.environmentId, environmentId),
      );
    expect(savedRows).toHaveLength(1);
  });

  it("refuses to publish against a superseded Saved revision", async () => {
    await save("First revision");
    const staleReview = await publicationReview();
    await save("Second revision");

    const result = await harness.runTransaction(() =>
      saveManualEnvironmentStateSnapshot(
        {
          environmentId,
          actorId: userId,
          message: "Stale revision",
          review: staleReview,
        }
      ),
    );

    expect(result.status).toBe("error");
    if (result.status === "error") {
      expect(result.error.message).toContain(
        "Saved State changed after this action was reviewed",
      );
    }
    expect(
      await harness.db
        .select()
        .from(schema.environmentSavedStateSnapshot)
        .where(
          eq(
            schema.environmentSavedStateSnapshot.environmentId,
            environmentId,
          ),
        ),
    ).toHaveLength(2);
  });

  it("rejects a stale user-reviewed Working revision without writing", async () => {
    const reviewedWorkingStateFingerprint = await reviewFingerprint();
    const savedStateBasis = await currentSavedStateBasis();

    await harness.db
      .update(schema.environment)
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,name}', to_jsonb(${"Changed concurrently"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));

    await expect(harness.runTransaction(() =>
      createManualEnvironmentDeployment(
        {
          environmentId,
          actorId: userId,
          message: "Reviewed state",
          review: {
            workingStateFingerprint: reviewedWorkingStateFingerprint,
            savedStateBasis,
            destructiveServiceIds: [],
            destructiveVolumeReviews: [],
          },
        },
      ),
    )).rejects.toMatchObject({
      message:
        "Working State changed after the manual deployment was reviewed.",
    });

    expect(
      await harness.db.select().from(schema.environmentSavedStateSnapshot),
    ).toEqual([]);
    expect(
      await harness.db.select().from(schema.environmentDeployment),
    ).toEqual([]);
  });

  it("rejects a stale destructive Save review without publishing Saved State", async () => {
    const reviewedWorkingStateFingerprint = await reviewFingerprint();
    const savedStateBasis = await currentSavedStateBasis();
    await harness.db
      .update(schema.environment)
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,name}', to_jsonb(${"Changed after Save review"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));

    const result = await harness.runTransaction(() =>
      saveManualEnvironmentStateSnapshot(
        {
          environmentId,
          actorId: userId,
          message: "Reviewed destructive Save",
          review: {
            workingStateFingerprint: reviewedWorkingStateFingerprint,
            savedStateBasis,
            destructiveServiceIds: [],
            destructiveVolumeReviews: [],
          },
        }
      ),
    );

    expect(result.status).toBe("error");
    if (result.status === "error") {
      expect(result.error.message).toBe(
        "Working State changed after the Save was reviewed.",
      );
    }
    expect(
      await harness.db.select().from(schema.environmentSavedStateSnapshot),
    ).toEqual([]);
  });

  it("rejects an unreviewed destructive transition at the Saved publication boundary", async () => {
    const deployment = await deploy("Applied baseline");
    await harness.runEffect(
      markDeploymentStatus({
        environmentDeploymentId: deployment.environmentDeploymentId,
        status: "applied",
      }).pipe(
        Effect.provideService(SecretEncryption, encryption),
        Effect.provideService(InngestClient, inngest),
      ),
    );
    await harness.db
      .update(schema.environment)
      .set({ intent: { version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [] }, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));
    const review = await publicationReview();

    const result = await harness.runTransaction(() =>
      saveManualEnvironmentStateSnapshot(
        {
          environmentId,
          actorId: userId,
          message: "Automated publication",
          review,
        }
      ),
    );

    expect(result.status).toBe("error");
    if (result.status === "error") {
      expect(result.error.message).toContain("changed after review");
    }
  });

  it("keeps the reviewed revision fixed across a repeatable-read retry", async () => {
    const reviewedWorkingStateFingerprint = await reviewFingerprint();
    let attempt = 0;
    const retryingDb: DatabaseService = {
      drizzle: harness.database.drizzle,
      transaction: ((program, config) =>
        Effect.suspend(() => {
          attempt += 1;
          if (attempt === 1) {
            const serializationFailure = new SqlError({
              reason: new SerializationError({
                cause: new Error("retry reviewed deployment"),
                message: "retry reviewed deployment",
              }),
            });
            return harness.database
              .transaction(
                program.pipe(
                  Effect.flatMap(() => Effect.fail(serializationFailure)),
                ),
                config,
              )
              .pipe(
                Effect.catch((cause) =>
                harness.database.drizzle
                    .update(schema.environment)
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,name}', to_jsonb(${"Changed between retries"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId))
                    .pipe(Effect.flatMap(() => Effect.fail(cause))),
                ),
              );
          }
          return harness.database.transaction(program, config);
        })) as DatabaseService["transaction"],
    };

    const result = await Effect.runPromise(
      withMutationReceipt(
        createManualEnvironmentDeployment({
          environmentId,
          actorId: userId,
          message: "Reviewed state",
          review: {
            workingStateFingerprint: reviewedWorkingStateFingerprint,
            savedStateBasis: { kind: "no_saved_state" },
            destructiveServiceIds: [],
            destructiveVolumeReviews: [],
          },
        }),
        { isolationLevel: "repeatable read" },
      ).pipe(
        Effect.provideService(Database, retryingDb),
        Effect.exit,
      ),
    );

    expect(Exit.isFailure(result)).toBe(true);
    expect(
      await harness.db.select().from(schema.environmentSavedStateSnapshot),
    ).toEqual([]);
    expect(
      await harness.db.select().from(schema.environmentDeployment),
    ).toEqual([]);
  });

  it("refreshes one queued target from a new Saved revision while preserving earlier Saved evidence", async () => {
    const first = await deploy("First target");
    const [firstNode] = await harness.db
      .select({ config: schema.environmentNodeConfigSnapshot.config })
      .from(schema.environmentNodeConfigSnapshot)
      .where(
        eq(
          schema.environmentNodeConfigSnapshot.environmentDeploymentId,
          first.environmentDeploymentId,
        ),
      );

    await harness.db
      .update(schema.environment)
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,name}', to_jsonb(${"Changed target"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));
    const second = await deploy("Second target");

    const savedRows = await harness.db
      .select({
        intent: schema.environmentSavedStateSnapshot.intent,
      })
      .from(schema.environmentSavedStateSnapshot)
      .where(
        eq(schema.environmentSavedStateSnapshot.environmentId, environmentId),
      );
    const queuedNodes = await harness.db
      .select({ config: schema.environmentNodeConfigSnapshot.config })
      .from(schema.environmentNodeConfigSnapshot)
      .where(
        eq(
          schema.environmentNodeConfigSnapshot.environmentDeploymentId,
          second.environmentDeploymentId,
        ),
      );

    expect(second.environmentDeploymentId).toBe(first.environmentDeploymentId);
    expect(firstNode?.config).toEqual(expect.objectContaining({ name: "API" }));
    expect(savedRows).toHaveLength(2);
    expect(
      savedRows.map(
        (row) =>
          decodeStrict(savedEnvironmentIntentSchema, row.intent).services[0]?.config
            .name,
      ),
    ).toEqual(expect.arrayContaining(["API", "Changed target"]));
    expect(queuedNodes).toEqual([
      expect.objectContaining({
        config: expect.objectContaining({ name: "Changed target" }),
      }),
    ]);
  });

  it("freezes a started Attempt Target while later Working changes queue separately", async () => {
    const running = await deploy("Running target");
    await harness.db
      .update(schema.environmentDeployment)
      .set({ status: "planning" })
      .where(
        eq(schema.environmentDeployment.id, running.environmentDeploymentId),
      );
    await harness.db
      .update(schema.environment)
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,name}', to_jsonb(${"Later Working state"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));

    const queued = await deploy("Later target");
    const targets = await harness.db
      .select({
        deploymentId:
          schema.environmentNodeConfigSnapshot.environmentDeploymentId,
        config: schema.environmentNodeConfigSnapshot.config,
      })
      .from(schema.environmentNodeConfigSnapshot);

    expect(queued.environmentDeploymentId).not.toBe(
      running.environmentDeploymentId,
    );
    expect(targets).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          deploymentId: running.environmentDeploymentId,
          config: expect.objectContaining({ name: "API" }),
        }),
        expect.objectContaining({
          deploymentId: queued.environmentDeploymentId,
          config: expect.objectContaining({ name: "Later Working state" }),
        }),
      ]),
    );
  });

  it("rolls back a saved row when its repeatable-read transaction fails", async () => {
    const result = await Effect.runPromise(
      withMutationReceipt(
        Effect.gen(function* () {
          const review = yield* Effect.promise(publicationReview);
          yield* saveManualEnvironmentStateSnapshot({
            environmentId,
            actorId: userId,
            message: "Rollback",
            review,
          });
          return yield* Effect.fail(new Error("injected failure"));
        }),
        { isolationLevel: "repeatable read" },
      ).pipe(
        Effect.provideService(Database, harness.database),
        Effect.exit,
      ),
    );
    expect(Exit.isFailure(result)).toBe(true);
    const rows = await harness.db
      .select()
      .from(schema.environmentSavedStateSnapshot);
    expect(rows).toEqual([]);
  });

  it("stages volume destroy from exact Saved authority", async () => {
    const baseline = await deploy("Applied baseline");
    await harness.runEffect(
      markDeploymentStatus({
        environmentDeploymentId: baseline.environmentDeploymentId,
        status: "applied",
      }).pipe(
        Effect.provideService(SecretEncryption, encryption),
        Effect.provideService(InngestClient, inngest),
      ),
    );

    await harness.db.insert(schema.resourceLineage).values({
      id: volumeLineageId,
      organizationId,
      projectId,
      canonicalName: "Data",
      canonicalSlug: "data",
    });
    await harness.db.insert(schema.environmentResource).values({
      id: volumeId,
      organizationId,
      projectId,
      environmentId,
      lineageId: volumeLineageId,
      implementationType: "volume",
    });
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values({
      organizationId,
      environmentDeploymentId: baseline.environmentDeploymentId,
      environmentId,
      nodeType: "volume",
      nodeId: volumeId,
      nodeLineageId: volumeLineageId,
      configVersion: 2,
      config: {
        version: 2,
        name: "Data",
        storage: { kind: "plain" },
      },
    });

    const [saved] = await harness.db
      .select({ id: schema.environmentSavedStateSnapshot.id })
      .from(schema.environmentSavedStateSnapshot)
      .where(
        eq(
          schema.environmentSavedStateSnapshot.environmentId,
          environmentId,
        ),
      )
      .orderBy(desc(schema.environmentSavedStateSnapshot.createdAt))
      .limit(1);
    if (saved === undefined) {
      throw new Error("expected a saved state snapshot");
    }
    await harness.db
      .update(schema.environmentSavedStateSnapshot)
      .set({
        volumeDeletionAuthorizations: [
          {
            target: {
              version: 1,
              resourceId: volumeId,
              namespaceId: "production",
              volumeName: `vol-${volumeId}`,
              machineId: "machine-1",
            },
            evidence: {
              version: 1,
              fingerprint: "reviewed-volume-removal",
              reviewedAt: "2026-08-12T00:00:00.000Z",
              evidence: {
                namespaceId: "production",
                volumeName: `vol-${volumeId}`,
                machineId: "machine-1",
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
      })
      .where(eq(schema.environmentSavedStateSnapshot.id, saved.id));

    const next = await deploy("Deploy without volume destroy");
    const attempts = await harness.db
        .select({ id: schema.destructiveVolumeAttempt.id })
        .from(schema.destructiveVolumeAttempt)
        .where(
          eq(
            schema.destructiveVolumeAttempt.environmentDeploymentId,
            next.environmentDeploymentId,
          ),
        );
    expect(attempts).toHaveLength(1);
  });
});
