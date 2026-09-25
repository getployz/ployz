import { testConfigEnvironment } from "#/test/config-environment";
import { deploymentReporting } from "./deployment-reporting.server";
import { preparationProgressCollector } from "./preparation-progress";
import { loadBuildReceipts, persistBuildReceipts } from "./build-receipts.server";
import type { DeploymentContext } from "./runtime-repository.contract";
import { resolveLogFilter } from "#/modules/runtime/container-logs.server";
import { Header } from "tar";
import { gzipSync } from "node:zlib";
import { access } from "node:fs/promises";
import { Schema } from "effect";
import { createImageServiceSource, createGitServiceSource, projectServiceDeploymentConfig, createDefaultServiceHealthcheck, createDefaultServiceRestartPolicy } from "#/modules/environment-design/services";
import { GithubApi } from "#/modules/github/github-observation.api";
import { asTestDouble } from "#/lib/test-double";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { makeOrganizationRuntimeLayer } from "#/modules/runtime/organization-runtime.server";
import { noPairingChanges } from "#/test/organization-runtime";
import { cleanUpDeploymentImages, executeEnvironmentDeployment, executeLatestEnvironmentDeployment } from "./runtime-activities.server";
import { markDeploymentCancelled, requestDeploymentCancellation } from "./runtime-cancellation.repository.server";
import { loadDeploymentBuildLog, loadDeploymentEvents, persistBuildLog, persistDeploymentProgress } from "./deployment-events.server";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { eq, sql } from "drizzle-orm";
import { Database } from "#/server/database.server";
import { ConfigProvider, Effect, Layer, Redacted } from "effect";
import { AppConfig } from "#/server/config.server";
import type { Client, ConfirmOptions, PreparedDeploy, ContainerId, DeployOutcome, ExecutionError } from "@ployz/sdk";
import { resolvedServiceSpecFixture, runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { readCollection } from "#/collections/read.server";
import { collectionReadInput } from "#/collections/read.contract";
import { Inngest } from "inngest";
import * as schema from "#/db/schema";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";
import {
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
// Hosted DNS points at a closed port, so an inline reserve fails fast.
const appConfig = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: {
  ...testConfigEnvironment(), DATABASE_URL: "postgres://unused.example.test/db", PLOYZ_HOSTED_DNS_URL: "http://127.0.0.1:9/",
} }))));

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
  let harness: PostgresTestHarness;

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  afterEach(async () => {
    await harness.pool.query("ALTER TABLE environment_deployment_event DROP CONSTRAINT IF EXISTS reject_test_event");
    await harness.pool.query("ALTER TABLE environment_deployment_build_step DROP CONSTRAINT IF EXISTS reject_test_step");
    await harness.pool.query("DROP TRIGGER IF EXISTS stall_reporting ON environment_deployment_event");
    await harness.pool.query("DROP FUNCTION IF EXISTS stall_reporting()");
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
        'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"volumes":[]}'
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

  it.each(["valid", "corrupt ciphertext", "invalid JSON", "incompatible schema", "rotated key", "reporting unavailable", "reporting stalls", "reporting recovers"])(
    "recovers and retains private build receipts across Git deployments: %s", async (evidence) => {
    if (evidence === "reporting unavailable" || evidence === "reporting recovers") {
      await harness.pool.query("ALTER TABLE environment_deployment_event ADD CONSTRAINT reject_test_event CHECK (false)");
      await harness.pool.query("ALTER TABLE environment_deployment_build_step ADD CONSTRAINT reject_test_step CHECK (false)");
    }
    if (evidence === "reporting stalls") {
      await harness.pool.query("CREATE FUNCTION stall_reporting() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(10); RETURN NEW; END $$");
      await harness.pool.query("CREATE TRIGGER stall_reporting BEFORE INSERT ON environment_deployment_event FOR EACH ROW EXECUTE FUNCTION stall_reporting()");
    }
    const receipt = { api: {
      fingerprint: "b".repeat(64), machine_id: runtimeWatchMachineFixture("a".repeat(32), "builder").id,
      image: { reference: `sha256:${"c".repeat(64)}`, tags: [], platforms: ["linux/amd64"], location: "unix:///var/run/docker.sock" },
    } };
    const git = projectServiceDeploymentConfig({
      source: createGitServiceSource({ repository: "owner/repo", repositoryId: 42, access: { type: "public" } }),
      privateDns: "api", preDeployCommand: null, startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy(),
    });
    const header = new Header({ path: "root/Dockerfile", size: 0, mode: 0o644, type: "File" });
    header.encode();
    if (!header.block) throw new Error("Archive fixture failed");
    const archive = gzipSync(Buffer.concat([Buffer.from(header.block), Buffer.alloc(1024)]));
    let attempt = 0;
    let confirmed = 0;
    const outcome = { type: "success" as const, completed: [] };
    const client = asTestDouble<Client>()({
      prepare: (input: Parameters<Client["prepare"]>[0]) => {
        expect(input.source_commits).toEqual({ api: "a".repeat(40) });
        expect(input.build_receipts).toEqual(attempt === 0 || (attempt === 1 && (evidence !== "valid" && !evidence.startsWith("reporting"))) ? {} : receipt);
        const prepared = asTestDouble<PreparedDeploy>()({
          ...preview(), buildReceipts: receipt, pruneTargets: [], close: () => undefined,
          confirm: () => {
            confirmed++;
            return { abort: () => undefined, finished: Promise.resolve(outcome),
              async *[Symbol.asyncIterator]() { yield { type: "outcome" as const, outcome }; } };
          },
        });
        return { abort: () => undefined, finished: Promise.resolve(prepared), async *[Symbol.asyncIterator]() {
          if (evidence === "reporting recovers" && attempt === 0) {
            await harness.pool.query("ALTER TABLE environment_deployment_event DROP CONSTRAINT reject_test_event");
            await harness.pool.query("ALTER TABLE environment_deployment_build_step DROP CONSTRAINT reject_test_step");
            await new Promise((resolve) => setTimeout(resolve, 1_100));
          }
          yield { Build: { Stage: "Upload" } };
          yield { Build: { Stage: "Building" } };
        } };
      },
      close: async () => undefined,
    });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant", connections: [{ management: "ployz1:test" }] }), noPairingChanges)
      .pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    for (const image of ["redis:7", "redis:8", "redis:9"]) {
      const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
        environmentId, savedStateSnapshotId: targetSavedId,
        triggerOrigin: { origin: "manual", actorId: userId }, message: null,
      }));
      await harness.db.update(schema.environmentDeployment).set({ status: "planning" }).where(eq(schema.environmentDeployment.id, admitted.id));
      const context: DeploymentContext = {
        deployment: { id: admitted.id, environmentId, status: "planning", inngestRunId: null, sourcePins: { [apiNodeId]: { commitSha: "a".repeat(40) } } },
        environment: { id: environmentId, namespace: "production" }, project: { id: projectId, organizationId }, organization: { id: organizationId, slug: "runtime" },
        snapshots: [{ serviceId: apiNodeId, serviceSlug: "api", config: git }, { serviceId: workerNodeId, serviceSlug: "worker", config: { ...git, privateDns: "worker", source: createImageServiceSource({ image }) } }], volumes: [],
      };
      await harness.runEffect(executeEnvironmentDeployment(context).pipe(
        Effect.provide(runtime),
        Effect.provideService(GithubApi, {
          json: (request) => Schema.decodeUnknownEffect(request.schema)({ id: 42, full_name: "owner/repo", private: false }).pipe(Effect.orDie),
          archive: () => Effect.succeed(new Response(archive)),
        }), Effect.provideService(InngestClient, new Inngest({ id: "test" })), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig),
      ));
      const [completed] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
      expect(completed?.status).toBe("applied");
      if (evidence.startsWith("reporting") && attempt === 0) expect(completed?.runtimeProgress?.logsIncomplete).toBe(true);
      expect(completed?.runtimeProgress?.outcome).toBe("success");
      if (evidence === "reporting recovers") {
        const logs = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId: admitted.id, after: 0, limit: 50 }));
        expect(logs.steps.some((step) => step.key === "stage:Upload")).toBe(true);
      }
      const [secret] = await harness.db.select().from(schema.environmentDeploymentSecret).where(eq(schema.environmentDeploymentSecret.environmentDeploymentId, admitted.id));
      expect(secret?.encryptedBuildReceipts).toBeTruthy();
      if (!secret?.encryptedBuildReceipts) throw new Error("Missing build evidence");
      expect(JSON.parse(encryption.decrypt(secret.encryptedBuildReceipts))).toEqual(receipt);
      expect(JSON.stringify(secret)).not.toContain(receipt.api.fingerprint);
      // Receipt writes cannot mutate a completed or differently owned attempt.
      await expect(harness.runEffect(persistBuildReceipts(context, receipt).pipe(Effect.provideService(SecretEncryption, encryption))))
        .rejects.toMatchObject({ failureCode: "build_receipts_not_owned" });
      expect(await harness.runEffect(loadBuildReceipts({ ...context, organization: { id: userId, slug: "other" } }).pipe(Effect.provideService(SecretEncryption, encryption)))).toEqual({});
      if (attempt === 0 && (evidence !== "valid" && !evidence.startsWith("reporting"))) {
        const unreadable = evidence === "invalid JSON" ? encryption.encrypt("{")
          : evidence === "incompatible schema" ? encryption.encrypt(JSON.stringify({ api: { ...receipt.api, version: 2 } }))
          : evidence === "rotated key" ? makeSecretEncryption("previous-encryption-secret").encrypt(JSON.stringify(receipt))
          : { ...secret.encryptedBuildReceipts, ciphertext: "corrupt" };
        await harness.db.update(schema.environmentDeploymentSecret)
          .set({ encryptedBuildReceipts: unreadable })
          .where(eq(schema.environmentDeploymentSecret.environmentDeploymentId, admitted.id));
      }
      attempt++;
    }
    expect(confirmed).toBe(3);
  });

  it("scopes log discovery by organization and stable service identity", async () => {
    const filter = await harness.runTransaction(() => resolveLogFilter(organizationId, {
      organizationSlug: "runtime", environmentSlug: "production", serviceId: apiNodeId,
    }));
    expect(filter).toEqual({ projectName: "production", serviceId: apiNodeId, deploymentId: undefined });
    await expect(harness.runTransaction(() => resolveLogFilter("00000000-0000-4000-8000-000000000999", {
      organizationSlug: "other", environmentSlug: "production",
    }))).rejects.toThrow("Environment was not found");
  });

  it.each(["failed", "unknown", "cancelled", "progress-storage"] as const)("preparation %s cleans source and never confirms or applies", async (kind) => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    await harness.db.update(schema.environmentDeployment).set({ status: "planning" }).where(eq(schema.environmentDeployment.id, admitted.id));
    await harness.pool.query('insert into member(user_id,organization_id) values($1,$2)', [userId, organizationId]);
    await harness.pool.query("insert into github_installation(user_id,installation_id,account_login,account_type) values($1,17,'owner','User')", [userId]);
    await harness.pool.query("insert into github_repository_cache(user_id,installation_id,repository_id,name,full_name,default_branch,private,html_url,repo_updated_at) values($1,17,42,'repo','owner/repo','main',true,'https://github.com/owner/repo',now())", [userId]);
    if (kind === "progress-storage") {
      await harness.pool.query(`ALTER TABLE environment_deployment_build_step ADD CONSTRAINT reject_test_progress CHECK (key <> 'stage:Upload')`);
    }
    const header = new Header({ path: "root/Dockerfile", size: 0, mode: 0o644, type: "File" });
    header.encode();
    if (!header.block) throw new Error("Archive fixture failed");
    const archive = gzipSync(Buffer.concat([Buffer.from(header.block), Buffer.alloc(1024)]));
    let checkout: string | undefined;
    let confirmed = 0;
    await harness.db.insert(schema.organizationClusterDomain).values({
      organizationId, endpoint: "https://dns.example.test/", name: "cluster.example.test",
      encryptedToken: encryption.encrypt("token"), reservedAt: new Date(), leaseRenewedAt: new Date(),
    });
    const client = asTestDouble<Client>()({
      prepare: (input: Parameters<Client["prepare"]>[0]) => {
        expect(input.deployment.snapshots[0]?.resolvedEnv?.["PLOYZ_PUBLIC_DOMAIN"]).toBe("api.cluster.example.test");
        expect(input.deployment.snapshots[0]?.config.managedHostnames).toEqual([]);
        expect(input.deployment.snapshots[0]?.config.routes.map((route) => route.hostname)).toEqual(["api.cluster.example.test"]);
        checkout = Object.values(input.sources)[0];
        const finished = Promise.reject({ code: "internal", details: { preparation: { kind: kind === "progress-storage" ? "cancelled" : kind, stage: "Building" } } });
        void finished.catch(() => undefined);
        return {
          abort: () => undefined,
          finished,
          async *[Symbol.asyncIterator]() { yield { Build: { Stage: kind === "progress-storage" ? "Upload" : "Building" } }; },
        };
      },
      preview: async () => { confirmed += 1; throw new Error("Preparation failure must never preview/confirm"); },
      close: async () => undefined,
    });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant", connections: [{ management: "ployz1:test" }] }), noPairingChanges)
      .pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    const config = projectServiceDeploymentConfig({ source: createGitServiceSource({ repository: "owner/repo", repositoryId: 42, access: { type: "github-installation", installationId: 17  }}),
      privateDns: "api", managedHostnames: [{ prefix: "api", targetPort: null }], preDeployCommand: null, startCommand: null, healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy() });
    await harness.runEffect(executeEnvironmentDeployment({
      deployment: { id: admitted.id, environmentId, status: "planning", inngestRunId: null, sourcePins: { [apiNodeId]: { commitSha: "a".repeat(40) } } },
      environment: { id: environmentId, namespace: "production" }, project: { id: projectId, organizationId }, organization: { id: organizationId, slug: "runtime" },
      snapshots: [{ serviceId: apiNodeId, serviceSlug: "api", config }], volumes: [],
    }).pipe(Effect.scoped, Effect.provide(runtime),
      Effect.provideService(GithubApi, {
        json: (request) => Schema.decodeUnknownEffect(request.schema)({ id: 42, full_name: "owner/repo" }).pipe(Effect.orDie),
        archive: () => Effect.succeed(new Response(archive)),
      }), Effect.provideService(InngestClient, new Inngest({ id: "test" })), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig), Effect.result));
    if (kind === "progress-storage") await harness.pool.query("ALTER TABLE environment_deployment_build_step DROP CONSTRAINT reject_test_progress");
    expect(confirmed).toBe(0);
    expect(checkout).toBeDefined();
    if (checkout) await expect(access(checkout)).rejects.toThrow();
    const [attempt] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(attempt?.status).toBe(kind === "cancelled" || kind === "progress-storage" ? "cancelled" : "failed");
    expect(attempt?.failureCode).toBe(`sdk_preparation_${kind === "progress-storage" ? "cancelled" : kind}`);
    if (kind === "progress-storage") expect(attempt?.runtimeProgress?.logsIncomplete).toBe(true);
    expect(attempt?.finishedAt).toBeInstanceOf(Date);
    expect(attempt?.deployPreview).toBeNull();
    if (kind !== "progress-storage") expect(attempt?.runtimeProgress?.preparation?.phase).toBe("build");
  });

  it("refuses a managed hostname deploy when the inline Cluster Domain reserve fails", async () => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    await harness.db.update(schema.environmentDeployment).set({ status: "planning" }).where(eq(schema.environmentDeployment.id, admitted.id));
    let previewed = false;
    const client = asTestDouble<Client>()({ preview: async () => { previewed = true; throw new Error("A refused deploy must never preview"); }, close: async () => undefined });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant", connections: [{ management: "ployz1:test" }] }), noPairingChanges)
      .pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    const config = projectServiceDeploymentConfig({ source: createImageServiceSource({ image: "nginx:1" }),
      privateDns: "api", managedHostnames: [{ prefix: "api", targetPort: null }], preDeployCommand: null, startCommand: null, healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy() });
    const result = await harness.runEffect(executeEnvironmentDeployment({
      deployment: { id: admitted.id, environmentId, status: "planning", inngestRunId: null, sourcePins: {} },
      environment: { id: environmentId, namespace: "production" }, project: { id: projectId, organizationId }, organization: { id: organizationId, slug: "runtime" },
      snapshots: [{ serviceId: apiNodeId, serviceSlug: "api", config }], volumes: [],
    }).pipe(Effect.scoped, Effect.provide(runtime),
      Effect.provideService(GithubApi, { json: () => Effect.die("Image deploy must not fetch Git"), archive: () => Effect.die("Image deploy must not fetch Git") }),
      Effect.provideService(InngestClient, new Inngest({ id: "test" })), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig), Effect.flip));
    expect(result).toMatchObject({ _tag: "DeploymentExecutionError", failureCode: "cluster_domain_unreserved" });
    expect(result.message).toContain("Server Settings");
    expect(previewed).toBe(false);
  });

  it.each(["outcome", "rejection"])("settles a cancelled quiet runner after runtime %s", async (completion) => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    await harness.db.update(schema.environmentDeployment).set({ status: "planning", startedAt: new Date() }).where(eq(schema.environmentDeployment.id, admitted.id));
    let aborted = false;
    let closed = false;
    let started = false;
    let finish: () => void = () => undefined;
    const stopped = new Promise<void>((resolve) => { finish = resolve; });
    const operation = { type: "run_container" as const,
      machine_id: runtimeWatchMachineFixture("a".repeat(32), "machine").id,
      spec: resolvedServiceSpecFixture(), skip_health_monitor: false };
    const outcome: DeployOutcome<ExecutionError> = { type: "failed", completed: [],
      failed: { type: "operation", operation, error: { type: "cancelled" } }, unexecuted: [] };
    const prepared = asTestDouble<PreparedDeploy>()({
      ...preview(), operations: [{ index: 0, operation, machine_id: operation.machine_id,
        service_name: operation.spec.name, machine_name: null, display_name: null, status: { type: "pending" } }],
      pruneTargets: [], confirm: () => ({
        abort: () => { aborted = true; },
        finished: stopped.then(() => outcome),
        async *[Symbol.asyncIterator]() {
          started = true;
          await stopped;
          if (completion === "rejection") throw new Error("Runtime disconnected during cancellation");
          yield { type: "outcome" as const, outcome };
        },
      }),
    });
    const client = asTestDouble<Client>()({ preview: async () => prepared, close: async () => { closed = true; } });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "grant-1", connections: [{ management: "ployz1:candidate" }],
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    const result = harness.runEffect(Effect.scoped(executeLatestEnvironmentDeployment(admitted.id)).pipe(
      Effect.provide(runtime), Effect.provideService(GithubApi, { json: () => Effect.die("Image deploy must not fetch Git"), archive: () => Effect.die("Image deploy must not fetch Git") }), Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig),
    ));
    const settled = result.then(value => ({ value }), error => ({ error }));
    try {
      await vi.waitFor(() => expect(started).toBe(true));
      expect(await harness.runEffect(requestDeploymentCancellation(admitted.id).pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" }))))).toBe(false);
      await vi.waitFor(() => expect(aborted).toBe(true), { timeout: 2000 });
      const [cancelling] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
      expect(cancelling?.status).toBe("deploying");
      expect(cancelling?.cancellationRequestedAt).toBeInstanceOf(Date);
      expect(cancelling?.finishedAt).toBeNull();
      expect(closed).toBe(false);
      finish();
      if (completion === "rejection") {
        expect(await settled).toMatchObject({ error: { _tag: "PloyzProviderError" } });
      } else {
        expect(await settled).toEqual({ value: { type: "failed", completed: 0, unexecuted: 0, reason: "cancelled" } });
      }
      await harness.runEffect(markDeploymentCancelled({ deploymentId: admitted.id }, "Runtime cancelled.").pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" }))));
      const [row] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
      expect(row?.finishedAt).toBeInstanceOf(Date);
      const [secret] = await harness.db.select().from(schema.environmentDeploymentSecret);
      if (completion === "rejection") {
        expect(row).toMatchObject({ status: "failed", failureCode: "sdk_deploy_outcome_unknown" });
        expect(secret?.encryptedRuntimeOutcome).toBeFalsy();
      } else {
        expect(row?.status).toBe("cancelled");
        expect(row?.runtimeProgress?.outcome).toBe("failed");
        expect(secret?.encryptedRuntimeOutcome).toBeTruthy();
      }
      expect(closed).toBe(true);
    } finally {
      finish();
      await settled;
    }
  });

  it("settles cancellation between planning and confirmation without runtime effects", async () => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId, triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    await harness.db.update(schema.environmentDeployment).set({ status: "planning", cancellationRequestedAt: new Date() })
      .where(eq(schema.environmentDeployment.id, admitted.id));
    const confirm = vi.fn(() => { throw new Error("Cancelled work must not execute"); });
    const client = asTestDouble<Client>()({ preview: async () => asTestDouble<PreparedDeploy>()({ ...preview(), pruneTargets: [], confirm }), close: async () => {} });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant-1", connections: [{ management: "ployz1:candidate" }] }), noPairingChanges)
      .pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    const result = await harness.runEffect(Effect.scoped(executeLatestEnvironmentDeployment(admitted.id)).pipe(
      Effect.provide(runtime), Effect.provideService(GithubApi, { json: () => Effect.die("Image deploy must not fetch Git"), archive: () => Effect.die("Image deploy must not fetch Git") }), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig), Effect.provideService(InngestClient, new Inngest({ id: "cancel-test" })),
    ));
    expect(result).toMatchObject({ type: "failed", reason: "cancelled", completed: 0 });
    expect(confirm).not.toHaveBeenCalled();
    expect((await harness.db.select().from(schema.environmentDeployment))[0]).toMatchObject({ status: "cancelled", finishedAt: expect.any(Date) });
  });

  it.each(["success", "cancelled"])("holds the execution slot through cleanup before persisting %s", async (resultKind) => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId, triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    await harness.db.update(schema.environmentDeployment).set({ status: "planning" }).where(eq(schema.environmentDeployment.id, admitted.id));
    let release: () => void = () => undefined;
    let closing: () => void = () => undefined;
    const cleanup = new Promise<void>((resolve) => { release = resolve; });
    const closeStarted = new Promise<void>((resolve) => { closing = resolve; });
    const outcome = { type: "success" as const, completed: [] };
    const confirm = vi.fn(() => ({ finished: Promise.resolve(outcome), abort: () => undefined,
      async *[Symbol.asyncIterator]() { yield { type: "outcome" as const, outcome }; },
    }));
    const client = asTestDouble<Client>()({
      preview: async () => {
        if (resultKind === "cancelled") await harness.db.update(schema.environmentDeployment).set({ cancellationRequestedAt: new Date() }).where(eq(schema.environmentDeployment.id, admitted.id));
        return asTestDouble<PreparedDeploy>()({ ...preview(), pruneTargets: [], confirm });
      },
      close: async () => { closing(); await cleanup; },
    });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant", connections: [{ management: "ployz1:test" }] }), noPairingChanges)
      .pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    const running = harness.runEffect(executeLatestEnvironmentDeployment(admitted.id).pipe(Effect.scoped, Effect.provide(runtime),
      Effect.provideService(GithubApi, { json: () => Effect.die("No Git expected"), archive: () => Effect.die("No Git expected") }),
      Effect.provideService(InngestClient, new Inngest({ id: "cleanup-test" })), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig)));
    try {
      await closeStarted;
      const [active] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
      expect(active?.status).toBe("deploying");
      expect(active?.finishedAt).toBeNull();
      expect(confirm).toHaveBeenCalledTimes(resultKind === "success" ? 1 : 0);
    } finally { release(); }
    await running;
    const [terminal] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(terminal?.status).toBe(resultKind === "success" ? "applied" : "cancelled");
  });

  it.each(["cleaned", "warning"] as const)("records %s Image Cleanup after the slot is released without changing status", async (expected) => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId, triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    await harness.db.update(schema.environmentDeployment).set({ status: "planning" }).where(eq(schema.environmentDeployment.id, admitted.id));
    const target = { machine_id: runtimeWatchMachineFixture("a".repeat(32), "machine").id, repository: "ployz-build/api" };
    const outcome = { type: "success" as const, completed: [] };
    const confirm = vi.fn((_options: ConfirmOptions) => ({ finished: Promise.resolve(outcome), abort: () => undefined,
      async *[Symbol.asyncIterator]() { yield { type: "outcome" as const, outcome }; },
    }));
    const pruneImages = vi.fn(async () => {
      if (expected === "warning") throw new Error("Server unreachable");
      return { machines: [{ machine_id: target.machine_id, result: { status: "cleaned" as const, removals: [] } }] };
    });
    const client = asTestDouble<Client>()({
      preview: async () => asTestDouble<PreparedDeploy>()({ ...preview(), pruneTargets: [target], confirm }),
      pruneImages, close: async () => {},
    });
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant", connections: [{ management: "ployz1:test" }] }), noPairingChanges)
      .pipe(Layer.provide(makePloyzLayer({ connect: async () => client })));
    const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.scoped, Effect.provide(runtime),
      Effect.provideService(GithubApi, { json: () => Effect.die("No Git expected"), archive: () => Effect.die("No Git expected") }),
      Effect.provideService(InngestClient, new Inngest({ id: "image-cleanup-test" })), Effect.provideService(SecretEncryption, encryption), Effect.provide(appConfig));
    await harness.runEffect(provide(executeLatestEnvironmentDeployment(admitted.id)));
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ imageCleanup: "manual" }));
    expect(pruneImages).not.toHaveBeenCalled();
    const [released] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(released).toMatchObject({ status: "applied", runtimeProgress: { imageCleanup: { state: "running", machines: 1, targets: [target] } } });
    await harness.runEffect(provide(cleanUpDeploymentImages(admitted.id)));
    expect(pruneImages).toHaveBeenCalledWith([target]);
    const [cleaned] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(cleaned?.status).toBe("applied");
    expect(cleaned?.runtimeProgress?.imageCleanup).toEqual({ state: expected, machines: 1 });
  });

  it("drops reporting promptly when its pool is full while the lifecycle pool remains usable", async () => {
    let release: () => void = () => undefined;
    const released = new Promise<void>((resolve) => { release = resolve; });
    let ready: () => void = () => undefined;
    const started = new Promise<void>((resolve) => { ready = resolve; });
    let connected = 0;
    const holders = [0, 1].map(() => Effect.runPromise(harness.reportingDatabase.transaction(Effect.gen(function* () {
      if (++connected === 2) ready();
      yield* Effect.promise(() => released);
    }))));
    try {
      await started;
      const reporting = deploymentReporting();
      const start = Date.now();
      await harness.runEffect(reporting.write(Effect.gen(function* () {
        const database = yield* Database;
        yield* database.transaction(database.drizzle.execute(sql`select 1`));
      })));
      expect(Date.now() - start).toBeLessThan(2_000);
      expect(reporting.incomplete).toBe(true);
      await harness.pool.query("select 1");
    } finally {
      release();
      await Promise.all(holders);
    }
  });

  it("retains live progress without updating the lifecycle row and scopes reads to the organization", async () => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    const progress = { completed: 0, total: 0, rows: [], outcome: null, compensation: [] };
    const owner = await harness.pool.connect();
    try {
      await owner.query("BEGIN");
      await owner.query("UPDATE environment_deployment SET failure_message = failure_message WHERE id = $1", [admitted.id]);
      await harness.runEffect(deploymentReporting().write(persistDeploymentProgress(admitted.id, progress)));
    } finally {
      await owner.query("ROLLBACK");
      owner.release();
    }
    const [row] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(row?.runtimeProgress).toBeNull();
    await harness.pool.query('insert into member(user_id,organization_id) values($1,$2)', [userId, organizationId]);
    const read = () => harness.runEffect(readCollection({ userId }, { table: "environment_deployment", userId, organizationSlug: "runtime" })).then((snapshot) => snapshot.rows);
    expect(await read()).toEqual(expect.arrayContaining([expect.objectContaining({ id: admitted.id, runtimeProgress: progress })]));
    const terminal = { ...progress, outcome: "success" as const, logsIncomplete: true };
    await harness.db.update(schema.environmentDeployment).set({ runtimeProgress: terminal }).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(await read()).toEqual(expect.arrayContaining([expect.objectContaining({ id: admitted.id, runtimeProgress: terminal })]));
    const page = await harness.runEffect(loadDeploymentEvents({ organizationId, deploymentId: admitted.id, after: 0 }));
    expect(page.events).toHaveLength(1);
    expect(page.events[0]?.progress).toEqual(progress);
    const first = page.events[0];
    if (!first) throw new Error("Missing retained progress");
    expect((await harness.runEffect(loadDeploymentEvents({ organizationId, deploymentId: admitted.id, after: first.id }))).events).toEqual([]);
    await expect(harness.runEffect(loadDeploymentEvents({ organizationId: userId, deploymentId: admitted.id, after: 0 }))).rejects.toThrow();
  });

  it("upserts build steps by key, attributes output to steps, and scopes reads to the organization", async () => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    const started = new Date("2026-09-22T21:09:06Z");
    const running = { build: 1, key: "sha256:a", name: "[sdk 5/6] RUN cargo build", startedAt: started, completedAt: null, cached: false, error: null };
    // Output may arrive for a step that has not been reported yet.
    await harness.runEffect(persistBuildLog(admitted.id, { steps: [], output: [{ build: 1, step: "sha256:a", stderr: true, text: "Compiling\n" }] }));
    await harness.runEffect(persistBuildLog(admitted.id, { steps: [running], output: [] }));
    await harness.runEffect(persistBuildLog(admitted.id, { steps: [running, { ...running, completedAt: new Date("2026-09-22T21:11:28Z") }], output: [{ build: 1, step: "sha256:a", stderr: false, text: "Finished\n" }] }));
    await harness.runEffect(persistBuildLog(admitted.id, { steps: [], output: [{ build: 1, step: "sha256:a", stderr: false, text: "Later output\n" }] }));
    const page = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId: admitted.id, after: 0, limit: 1 }));
    expect(page.steps.map(({ key, name, startedAt, completedAt }) => ({ key, name, startedAt, completedAt }))).toEqual([
      { key: "sha256:a", name: "[sdk 5/6] RUN cargo build", startedAt: started, completedAt: new Date("2026-09-22T21:11:28Z") },
    ]);
    expect(page.output.map((row) => [row.stepId, row.stderr, row.text])).toEqual([[page.steps[0]?.id, true, "Compiling\n"]]);
    expect(page.nextSequence).toBe(String(page.output[0]?.id));
    const rest = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId: admitted.id, after: Number(page.nextSequence), limit: 100 }));
    expect(rest.output.map((row) => row.text)).toEqual(["Finished\n", "Later output\n"]);
    expect(rest.output.every((row) => row.stepId === page.steps[0]?.id)).toBe(true);
    expect(rest.nextSequence).toBeNull();
    await expect(harness.runEffect(loadDeploymentBuildLog({ organizationId: userId, deploymentId: admitted.id, after: 0, limit: 100 }))).rejects.toThrow();
  });

  it("persists repeated client and daemon upload stages without restarting the clock", async () => {
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    let time = 1_000;
    const collector = preparationProgressCollector(() => new Date(time));
    await harness.runEffect(persistBuildLog(admitted.id, collector.event({ Build: { Stage: "Upload" } })));
    time = 2_000;
    await harness.runEffect(persistBuildLog(admitted.id, collector.event({ Build: { Stage: "Upload" } })));
    time = 3_000;
    await harness.runEffect(persistBuildLog(admitted.id, collector.event({ Build: { Stage: "Preparation" } })));
    const page = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId: admitted.id, after: 0, limit: 100 }));
    expect(page.steps.filter((row) => row.key === "stage:Upload")).toMatchObject([
      { build: 0, startedAt: new Date(1_000), completedAt: new Date(3_000) },
    ]);
  });

  it("retains a normally admitted outcome and applies the deployment", async () => {
    const admit = () => harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId, savedStateSnapshotId: targetSavedId,
      triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    const admitted = await admit();
    await expect(admit()).rejects.toMatchObject({ _tag: "Conflict" });
    await harness.db.update(schema.environmentDeployment).set({ deployPreview: preview() }).where(eq(schema.environmentDeployment.id, admitted.id));
    expect(await harness.db.select().from(schema.environmentDeploymentSecret)).toEqual([
      { organizationId, environmentDeploymentId: admitted.id, encryptedRuntimeOutcome: null, encryptedBuildReceipts: null },
    ]);
    const outcome = { version: 1, outcome: { type: "success" as const, completed: [] } };
    await harness.runEffect(persistSdkDeployOutcome({
      environmentDeploymentId: admitted.id, outcome: Redacted.make(outcome),
    }).pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })), Effect.provideService(SecretEncryption, encryption)));
    const privateRows = await harness.db.select().from(schema.environmentDeploymentSecret);
    expect(privateRows).toHaveLength(1);
    const encryptedOutcome = privateRows[0]?.encryptedRuntimeOutcome;
    if (!encryptedOutcome) throw new Error("Runtime evidence was not persisted");
    expect(JSON.parse(encryption.decrypt(encryptedOutcome))).toEqual(outcome);
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
      organizationId, environmentDeploymentId: targetDeploymentId,
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
    }).pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })), Effect.provideService(SecretEncryption, encryption)));
    const [privateRow] = await harness.db.select().from(schema.environmentDeploymentSecret)
      .where(eq(schema.environmentDeploymentSecret.environmentDeploymentId, targetDeploymentId));
    const encryptedOutcome = privateRow?.encryptedRuntimeOutcome;
    if (!encryptedOutcome) throw new Error("Runtime evidence was not persisted");
    expect(JSON.parse(encryption.decrypt(encryptedOutcome))).toEqual({ version: 1, outcome });
    const [publicRow] = await harness.db.select().from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(publicRow?.status).toBe("failed");
    expect(JSON.stringify([privateRow, publicRow])).not.toContain("never-publish-outcome");
    expect(collectionReadInput.fields.table.literals).not.toContain("environment_deployment_secret");
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
      }).pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })), Effect.provideService(SecretEncryption, encryption)),
    );
    const [previewRow] = await harness.db
      .select({ deployPreview: schema.environmentDeployment.deployPreview })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(previewRow?.deployPreview).toEqual(targetPreview);

    await harness.db.insert(schema.environmentDeploymentSecret).values({ organizationId, environmentDeploymentId: targetDeploymentId });
    await harness.runEffect(persistSdkDeployOutcome({ environmentDeploymentId: targetDeploymentId,
      outcome: Redacted.make({ version: 1, outcome: { type: "success", completed: [] } }),
    }).pipe(Effect.provideService(SecretEncryption, encryption), Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" }))));
    const projection = await harness.runEffect(
      loadEnvironmentSnapshotProjection(
        { kind: "environment", environmentId },
      ).pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })), Effect.provideService(SecretEncryption, encryption)),
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
    await harness.db.insert(schema.environmentDeploymentSecret).values({ organizationId, environmentDeploymentId: targetDeploymentId });
    for (const [id, lineageId, name] of [[apiNodeId, apiLineageId, "api"], [workerNodeId, workerLineageId, "worker"]] as const) {
      await harness.db.insert(schema.serviceLineage).values({ id: lineageId, organizationId, projectId, canonicalName: name, canonicalSlug: name });
      await harness.db.insert(schema.service).values({ id, organizationId, projectId, environmentId, lineageId, name });
      await harness.db.insert(schema.environmentNodeIntroduction).values({ organizationId, environmentId, nodeType: "service", nodeId: id, nodeLineageId: lineageId, config: {} });
    }
    await harness.db.update(schema.environmentDeployment).set({ inngestRunId: "owner" }).where(eq(schema.environmentDeployment.id, targetDeploymentId));
    const record = (expectedInngestRunId: string) => harness.runEffect(persistSdkDeployOutcome({
      environmentDeploymentId: targetDeploymentId, expectedInngestRunId, outcome: Redacted.make({ version: 1, outcome }),
    }).pipe(Effect.provideService(SecretEncryption, encryption), Effect.provideService(InngestClient, new Inngest({ id: "outcome-test" }))));
    await record("wrong-owner");
    expect((await harness.db.select().from(schema.environmentDeploymentSecret))[0]?.encryptedRuntimeOutcome).toBeNull();
    await record("owner");
    await record("owner");
    expect((await harness.db.select().from(schema.environmentNodeIntroduction)).map(row => row.nodeId)).toEqual([workerNodeId]);
    const services = await harness.db.select().from(schema.service);
    expect(services.find(row => row.id === apiNodeId)?.firstDeployedAt).toBeInstanceOf(Date);
    expect(services.find(row => row.id === workerNodeId)?.firstDeployedAt).toBeNull();
    expect((await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, targetDeploymentId)))[0]?.status).toBe("failed");

    const projection = await harness.runEffect(
      loadEnvironmentSnapshotProjection(
        { kind: "environment", environmentId },
      ).pipe(Effect.provideService(InngestClient, new Inngest({ id: "runtime-persistence-test" })), Effect.provideService(SecretEncryption, encryption)),
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
