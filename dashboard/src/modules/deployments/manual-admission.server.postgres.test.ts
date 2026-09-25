import { asTestDouble } from "#/lib/test-double";
import { runtimeWatchFrameFixture, runtimeWatchVolumeFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { OrganizationRuntime, type OrganizationRuntimeService, type ConnectedRuntimeClient } from "#/modules/runtime/organization-runtime.server";
import { submitReviewedPublication, prepareEnvironmentDestructiveVolumes } from "./deployment-command.server";
import { dispatchEnvironmentDeployment } from "./dispatch.server";
import { recordInngestRun } from "./runtime-lifecycle.repository.server";
import { createRetryAttempt } from "./retry-repository.server";
import { destructiveVolumeReviewsSchema } from "#/modules/environment-design/destructive-volume-review";
import { randomUUID } from "node:crypto";
import { parseServiceConfig } from "@ployz/sdk/config";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { desc, eq, sql } from "drizzle-orm";
import { Effect, Exit, Result } from "effect";
import { Inngest } from "inngest";
import { SqlError, SerializationError } from "effect/unstable/sql/SqlError";
import * as schema from "#/db/schema";
import { loadCurrentEnvironmentSnapshotProjection } from "#/modules/environment-design/working-state-repository.server";
import { buildEnvironmentChangeSet } from "#/modules/environment-design/environment-change-set";
import type { EnvironmentNodeProjection } from "#/modules/environment-design/environment-change-set";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { markDeploymentStatus } from "#/modules/deployments/runtime-repository.server";
import {
  createManualEnvironmentDeployment as admitManual,
} from "#/modules/deployments/deployment-command.server";
import { admitEnvironmentDeployment } from "#/modules/deployments/admission.server";
import { saveReviewedEnvironmentState } from "#/modules/environment-design/saved-state-operations.server";
import type { ReviewedEnvironmentPublication } from "#/modules/environment-design/working-state-review";
import { fingerprintReviewedEnvironmentWorkingState } from "#/modules/environment-design/working-state-review";
import { savedEnvironmentIntentSchema } from "#/modules/environment-design/saved-intent";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";
import type { DatabaseService } from "#/server/database.server";
import { Database } from "#/server/database.server";
import { withMutationResult } from "#/server/mutation-result.server";
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

function createManualEnvironmentDeployment(input: Parameters<typeof admitManual>[0]) {
  return admitManual(input).pipe(Effect.provideService(SecretEncryption, encryption));
}

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
  let harness: PostgresTestHarness;
  const inngest = new Inngest({ id: "manual-admission-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    vi.mocked(inngest.send).mockReset().mockResolvedValue({ ids: [] });
    const { env: _env, mounts: _mounts, ...config } = parseServiceConfig({ version: 2, source: { version: 1, type: "empty", rootDir: "/" }, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: "api" });

    const intent = { version: 1, environmentSlug: "production", volumes: [], services: [{
      id: serviceId, lineageId, slug: "api", config,
      variables: [{ id: variableId, key: "API_TOKEN", description: null, exported: false, valueFingerprint: "fingerprint", value: { kind: "secret", encryptedValue: null } }], volumeAttachments: [],
    }] };
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acceptance', 'acceptance');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        values ('${environmentId}', '${projectId}', '${organizationId}', 'Production', 'production', '${JSON.stringify(intent)}');
      insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug)
        values ('${lineageId}', '${organizationId}', '${projectId}', 'API', 'api');
      insert into service (id, project_id, environment_id, organization_id, lineage_id, name)
        values ('${serviceId}', '${projectId}', '${environmentId}', '${organizationId}', '${lineageId}', 'API');
      insert into service_registry_credential (organization_id, service_id, encrypted_registry_secret)
        values ('${organizationId}', '${serviceId}', '${JSON.stringify(encrypted)}');
      insert into variable (id, organization_id, environment_id, service_id)
        values ('${variableId}', '${organizationId}', '${environmentId}', '${serviceId}');
      insert into variable_secret (organization_id, environment_id, variable_id, encrypted_value)
        values ('${organizationId}', '${environmentId}', '${variableId}', '${JSON.stringify(encrypted)}');
    `);
    await harness.db.insert(schema.member).values({ id: randomUUID(), userId, organizationId, role: "owner" });
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

  function submit(deploy: boolean, review: ReviewedEnvironmentPublication, runtime: OrganizationRuntimeService = {
    cancel: () => Effect.void, open: () => Effect.die("Unexpected runtime call"),
  }) {
    return submitReviewedPublication({ userId }, {
      organizationSlug: "acceptance", projectSlug: "cloud", environmentSlug: "production",
      intent: deploy ? "manual_deploy" : "save", review,
    }).pipe(Effect.provideService(SecretEncryption, encryption), Effect.provideService(InngestClient, inngest),
      Effect.provideService(OrganizationRuntime, runtime));
  }

  it.each([false, true])("rejects incomplete, duplicate, and changed service reviews through the public command (deploy=%s)", async (shouldDeploy) => {
    const baseline = await deploy("Baseline");
    await harness.runEffect(markDeploymentStatus({ environmentDeploymentId: baseline.environmentDeploymentId, status: "applied" }).pipe(
      Effect.provideService(InngestClient, inngest), Effect.provideService(SecretEncryption, encryption),
    ));
    await harness.db.update(schema.environment).set({ intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));
    const review = await publicationReview();
    for (const destructiveServiceIds of [[], [serviceId, serviceId], [randomUUID()]]) {
      await expect(harness.runEffect(submit(shouldDeploy, { ...review, destructiveServiceIds }))).rejects.toMatchObject({ _tag: "Conflict" });
    }
    expect(await harness.db.select().from(schema.environmentSavedStateSnapshot)).toHaveLength(1);
    expect(await harness.db.select().from(schema.environmentDeployment)).toHaveLength(1);
    expect(inngest.send).not.toHaveBeenCalled();
    const outcome = await harness.runEffect(submit(shouldDeploy, { ...review, destructiveServiceIds: [serviceId] }));
    expect(outcome.state).toBe(shouldDeploy ? "deployment_queued" : "saved");
    expect(await harness.db.select().from(schema.environmentSavedStateSnapshot)).toHaveLength(2);
    const attempts = await harness.db.select().from(schema.environmentDeployment);
    expect(attempts).toHaveLength(shouldDeploy ? 2 : 1);
    // The publish outcome names the attempt it queued, so the canvas can open it.
    if (outcome.state === "deployment_queued") expect(attempts.map((attempt) => attempt.id)).toContain(outcome.deploymentId);
    expect(inngest.send).toHaveBeenCalledTimes(shouldDeploy ? 1 : 0);
  });

  it.each([false, true])("rejects stale public submissions without any publication (deploy=%s)", async (shouldDeploy) => {
    const review = await publicationReview();
    await harness.db.update(schema.environment).set({ revision: randomUUID() }).where(eq(schema.environment.id, environmentId));
    await expect(harness.runEffect(submit(shouldDeploy, review))).rejects.toMatchObject({ _tag: "Conflict" });
    expect(await harness.db.select().from(schema.environmentSavedStateSnapshot)).toEqual([]);
    expect(await harness.db.select().from(schema.environmentDeployment)).toEqual([]);
    expect(inngest.send).not.toHaveBeenCalled();
  });

  it("dispatches only after the outermost commit and drops dispatch on rollback", async () => {
    const review = await publicationReview();
    const command = Effect.gen(function* () {
      yield* submit(true, review);
      expect(inngest.send).not.toHaveBeenCalled();
    });
    await expect(harness.runTransaction(() => command.pipe(Effect.andThen(Effect.fail(new Error("rollback")))))).rejects.toThrow("rollback");
    expect(await harness.db.select().from(schema.environmentSavedStateSnapshot)).toEqual([]);
    expect(await harness.db.select().from(schema.environmentDeployment)).toEqual([]);
    expect(inngest.send).not.toHaveBeenCalled();
    vi.mocked(inngest.send).mockImplementation(async () => {
      expect(await harness.db.select().from(schema.environmentDeployment)).toHaveLength(1);
      return { ids: [] };
    });
    await harness.runTransaction(() => command);
    expect(inngest.send).toHaveBeenCalledTimes(1);
  });

  it("preserves a claimed worker when a delayed send fails", async () => {
    vi.mocked(inngest.send).mockImplementation(async () => {
      const [attempt] = await harness.db.select().from(schema.environmentDeployment);
      if (!attempt) throw new Error("Attempt was not committed");
      expect(await harness.runEffect(recordInngestRun({ environmentDeploymentId: attempt.id, runId: "claimed-before-send-failure" }))).toBe(true);
      throw new Error("late send error");
    });
    const outcome = await harness.runEffect(submit(true, await publicationReview()));
    const [attempt] = await harness.db.select().from(schema.environmentDeployment);
    expect(outcome).toEqual({ state: "deployment_queued", deploymentId: attempt?.id });
    expect(attempt).toMatchObject({ status: "queued", inngestRunId: "claimed-before-send-failure", finishedAt: null });
  });

  it.each([false, true])("keeps committed Saved State after dispatch failure and retries the exact revision explicitly (nested=%s)", async (nested) => {
    vi.mocked(inngest.send).mockRejectedValueOnce(new Error("send failed"));
    const command = submit(true, await publicationReview());
    if (nested) {
      await expect(harness.runTransaction(() => command)).rejects.toMatchObject({ _tag: "DatabasePostCommitFailure", cause: { _tag: "InngestEventSendError" } });
    } else {
      await expect(harness.runEffect(command)).resolves.toEqual({ state: "attempt_dispatch_failed" });
    }
    const [failed] = await harness.db.select().from(schema.environmentDeployment);
    if (!failed) throw new Error("missing attempt");
    expect(failed).toMatchObject({ status: "failed", failureCode: "inngest_dispatch_failed", finishedAt: expect.any(Date) });
    expect(await harness.db.select().from(schema.environmentSavedStateSnapshot)).toHaveLength(1);
    expect(inngest.send).toHaveBeenCalledTimes(1);
    await save("Newer Saved revision");
    await harness.runEffect(createRetryAttempt({ environmentId, userId, failedDeploymentId: failed.id }).pipe(Effect.provideService(InngestClient, inngest)));
    const rows = await harness.db.select().from(schema.environmentDeployment);
    expect(rows).toHaveLength(2);
    expect(rows.find(row => row.id !== failed.id)).toMatchObject({ savedStateSnapshotId: failed.savedStateSnapshotId, retryOfDeploymentId: failed.id });
    expect(inngest.send).toHaveBeenCalledTimes(2);
  });

  it("refuses automated replacement of an accepted manual target", async () => {
    const manual = await deploy("Reviewed manual target");
    await save("Newer Saved revision");
    const basis = await currentSavedStateBasis();
    if (basis.kind !== "saved_revision") throw new Error("missing Saved revision");
    await expect(harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: basis.savedStateSnapshotId,
      triggerOrigin: { origin: "first_connect", machineId: "a".repeat(32) }, message: null,
    }))).rejects.toMatchObject({ _tag: "Conflict" });
    const [row] = await harness.db.select().from(schema.environmentDeployment);
    expect(row).toMatchObject({ id: manual.environmentDeploymentId, triggerOrigin: { origin: "manual", actorId: userId } });
    expect(row?.savedStateSnapshotId).not.toBe(basis.savedStateSnapshotId);
  });

  it("freezes registry contents and revision across rotation and retry", async () => {
    const revision = randomUUID();
    const source = { version: 1, type: "image", image: "ghcr.io/acme/api:latest", credentials: { type: "configured", credentialId: serviceId } };
    await harness.db.update(schema.environment).set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,source}', ${JSON.stringify(source)}::jsonb)` }).where(eq(schema.environment.id, environmentId));
    await harness.db.insert(schema.serviceRegistryCredential).values({ organizationId, serviceId, revision, encryptedRegistrySecret: encrypted }).onConflictDoUpdate({ target: schema.serviceRegistryCredential.serviceId, set: { revision, encryptedRegistrySecret: encrypted } });
    const admitted = await deploy("Credential target");
    await harness.db.update(schema.serviceRegistryCredential).set({ revision: randomUUID(), encryptedRegistrySecret: encryption.encrypt("rotated") }).where(eq(schema.serviceRegistryCredential.serviceId, serviceId));
    await harness.db.update(schema.environmentDeployment).set({ status: "failed" }).where(eq(schema.environmentDeployment.id, admitted.environmentDeploymentId));
    await harness.runEffect(createRetryAttempt({ environmentId, userId, failedDeploymentId: admitted.environmentDeploymentId }).pipe(Effect.provideService(InngestClient, inngest)));
    const secrets = await harness.db.select().from(schema.environmentNodeConfigSnapshotSecret);
    expect(secrets).toHaveLength(2);
    expect(secrets.every(row => row.credentialRevision === revision)).toBe(true);
    expect(secrets.map(row => row.encryptedRegistrySecret)).toEqual([encrypted, encrypted]);
  });

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
    });
    expect(intent.services[0]).not.toHaveProperty("encryptedRegistrySecret");
    expect(
      await harness.db.select().from(schema.environmentDeployment),
    ).toEqual([]);
  });

  it("persists an empty-node saved state", async () => {
    await harness.db.update(schema.environment).set({ intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, revision: randomUUID() }).where(eq(schema.environment.id, environmentId));
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
    ).toMatchObject({ id: serviceId, config: { privateDns: "api" } });
    expect(queuedNodes).toEqual([
      expect.objectContaining({
        nodeType: "service",
        nodeId: serviceId,
        config: expect.objectContaining({ privateDns: "api" }),
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
        config: expect.objectContaining({ privateDns: "api" }),
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
      applied: { token: explicit.applied.token, nodes: [] },
      saved: { token: "saved", nodes },
      nodeIntroductions: { token: "introductions:none", nodes: [] },
      submitted: null,
    });

    expect(changeSet.groups).toMatchObject([
      {
        node: { type: "service", id: serviceId },
        lifecycle: "create",
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
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,startCommand}', to_jsonb(${"Changed concurrently"}::text))`, revision: randomUUID() })
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
        "Working State changed after publication was reviewed.",
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
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,startCommand}', to_jsonb(${"Changed after Save review"}::text))`, revision: randomUUID() })
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
        "Working State changed after publication was reviewed.",
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
      .set({ intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, revision: randomUUID() })
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
      afterCommit: harness.database.afterCommit,
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
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,startCommand}', to_jsonb(${"Changed between retries"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId))
                    .pipe(Effect.flatMap(() => Effect.fail(cause))),
                ),
              );
          }
          return harness.database.transaction(program, config);
        })) as DatabaseService["transaction"],
    };

    const result = await Effect.runPromise(
      withMutationResult(
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

  it("refuses a second queued manual command without publishing or changing its target", async () => {
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
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,startCommand}', to_jsonb(${"Changed target"}::text))`, revision: randomUUID() })
      .where(eq(schema.environment.id, environmentId));
    await expect(deploy("Second target")).rejects.toMatchObject({ _tag: "Conflict" });

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
          first.environmentDeploymentId,
        ),
      );

    expect(firstNode?.config).toEqual(expect.objectContaining({ privateDns: "api" }));
    expect(savedRows).toHaveLength(1);
    expect(
      savedRows.map(
        (row) =>
          decodeStrict(savedEnvironmentIntentSchema, row.intent).services[0]?.config
            .privateDns,
      ),
    ).toEqual(["api"]);
    expect(queuedNodes).toEqual([
      expect.objectContaining({
        config: expect.objectContaining({ privateDns: "api" }),
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
      .set({ intent: sql`jsonb_set(${schema.environment.intent}, '{services,0,config,startCommand}', to_jsonb(${"Later Working state"}::text))`, revision: randomUUID() })
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
          config: expect.objectContaining({ privateDns: "api" }),
        }),
        expect.objectContaining({
          deploymentId: queued.environmentDeploymentId,
          config: expect.objectContaining({ startCommand: "Later Working state" }),
        }),
      ]),
    );
  });

  it("rolls back a saved row when its repeatable-read transaction fails", async () => {
    const result = await Effect.runPromise(
      withMutationResult(
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

    const malformedAdmission = await harness.runEffect(
      Effect.result(
        admitEnvironmentDeployment({
          environmentId,
          savedStateSnapshotId: saved.id,
          triggerOrigin: { origin: "manual", actorId: userId },
          message: "Deploy without volume destroy",
          serviceActionPolicy: { kind: "all_affected_required" },
        }),
      ),
    );
    expect(Result.isFailure(malformedAdmission)).toBe(true);
    if (Result.isSuccess(malformedAdmission)) return;
    expect(malformedAdmission.failure).toMatchObject({
      _tag: "Validation",
      message: "Saved destructive volume review has an invalid machine ID.",
    });

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
      })
      .where(eq(schema.environmentSavedStateSnapshot.id, saved.id));

    const [authorizedSaved] = await harness.db.select().from(schema.environmentSavedStateSnapshot).where(eq(schema.environmentSavedStateSnapshot.id, saved.id));
    const review = await publicationReview();
    const next = await harness.runTransaction(() => createManualEnvironmentDeployment({
      environmentId, actorId: userId, message: "Deploy reviewed volume removal",
      review: { ...review, destructiveVolumeReviews: decodeStrict(destructiveVolumeReviewsSchema, authorizedSaved?.volumeDeletionAuthorizations ?? []) },
    }));
    const attempts = await harness.db
      .select({
        id: schema.volumeRemoveAttempt.id,
        status: schema.volumeRemoveAttempt.status,
        requestedByUserId: schema.volumeRemoveAttempt.requestedByUserId,
      })
      .from(schema.volumeRemoveAttempt)
      .where(
        eq(
          schema.volumeRemoveAttempt.environmentDeploymentId,
          next.environmentDeploymentId,
        ),
      );
    expect(attempts).toEqual([
      expect.objectContaining({
        status: "awaiting_deployment",
        requestedByUserId: userId,
      }),
    ]);
    vi.mocked(inngest.send).mockRejectedValueOnce(new Error("send failed"));
    await expect(harness.runEffect(dispatchEnvironmentDeployment({ environmentDeploymentId: next.environmentDeploymentId, environmentId })
      .pipe(Effect.provideService(InngestClient, inngest)))).rejects.toMatchObject({ _tag: "InngestEventSendError" });
    const [removal] = await harness.db.select().from(schema.volumeRemoveAttempt).where(eq(schema.volumeRemoveAttempt.environmentDeploymentId, next.environmentDeploymentId));
    expect(removal).toMatchObject({ status: "failed", terminalAt: expect.any(Date) });
    await expect(harness.runEffect(createRetryAttempt({ environmentId, userId, failedDeploymentId: next.environmentDeploymentId })
      .pipe(Effect.provideService(InngestClient, inngest)))).rejects.toMatchObject({ _tag: "Validation", message: expect.stringContaining("fresh destructive review") });
    expect(inngest.send).toHaveBeenCalledTimes(1);

    let observedMachine = "a".repeat(32);
    const runtime: OrganizationRuntimeService = {
      cancel: () => Effect.void,
      open: () => Effect.succeed({ status: "connected", connected: asTestDouble<ConnectedRuntimeClient>()({
        watchFirstFrame: () => Effect.succeed(runtimeWatchFrameFixture({
          volumes: [runtimeWatchVolumeFixture(observedMachine, `vol-${volumeId}`)],
        })),
      }) }),
    };
    const freshReviews = await harness.runEffect(prepareEnvironmentDestructiveVolumes({ userId }, {
      organizationSlug: "acceptance", projectSlug: "cloud", environmentSlug: "production",
    }).pipe(Effect.provideService(OrganizationRuntime, runtime), Effect.provideService(SecretEncryption, encryption)));
    expect(freshReviews).toHaveLength(1);
    const retryReview = await publicationReview();
    for (const shouldDeploy of [false, true]) {
      await expect(harness.runEffect(submit(shouldDeploy, retryReview, runtime))).rejects.toMatchObject({ _tag: "Conflict" });
      await expect(harness.runEffect(submit(shouldDeploy, { ...retryReview, destructiveVolumeReviews: [...freshReviews, ...freshReviews] }, runtime))).rejects.toMatchObject({ _tag: "Conflict" });
      observedMachine = "b".repeat(32);
      await expect(harness.runEffect(submit(shouldDeploy, { ...retryReview, destructiveVolumeReviews: freshReviews }, runtime)))
        .rejects.toMatchObject({ _tag: "DestructiveVolumeReviewChangedError" });
      observedMachine = "a".repeat(32);
    }
    expect(await harness.runEffect(submit(true, { ...retryReview, destructiveVolumeReviews: freshReviews }, runtime)))
      .toMatchObject({ state: "deployment_queued" });
    const removals = await harness.db.select().from(schema.volumeRemoveAttempt);
    expect(removals).toHaveLength(2);
    expect(removals.map(row => row.status).sort()).toEqual(["awaiting_deployment", "failed"]);
    expect(inngest.send).toHaveBeenCalledTimes(2);
  });
});
